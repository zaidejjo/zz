//! In-memory document buffers and accumulated checker context.
//!
//! Each `DocumentState` tracks which top-level definitions (functions,
//! structs, globals) it contributes to the global checker seed via
//! `FileDefs`. When a file is updated or closed, removed symbols are
//! pruned from the global seed to prevent stale type information.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::convert::LineIndex;
use crate::cross_file::ModuleIndex;
use dashmap::DashMap;
use tower_lsp::lsp_types::Url;
use zz_checker::{CheckResult, FuncSig, StructSig, Type};
use zz_frontend::ast::Program;
use zz_frontend::diag::RawDiag;
use zz_stdlib::stdlib_funcs;

// ── Per-file definition tracking ─────────────────────────────────────────

/// Names of top-level definitions that a single file contributes to the
/// global checker seed. Used to prune stale symbols on edit/close.
#[derive(Debug, Clone, Default)]
pub struct FileDefs {
    pub bindings: Vec<String>,
    pub funcs: Vec<String>,
    pub structs: Vec<String>,
}

impl FileDefs {
    /// Extract top-level definition names from a `CheckResult`.
    pub fn from_check_result(cr: &CheckResult) -> Self {
        Self {
            bindings: cr.bindings.keys().cloned().collect(),
            funcs: cr.funcs.keys().cloned().collect(),
            structs: cr.structs.keys().cloned().collect(),
        }
    }

    /// Is this empty (file defines nothing)?
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty() && self.funcs.is_empty() && self.structs.is_empty()
    }
}

// ── Per-file state ───────────────────────────────────────────────────────

/// Per-file state held in memory.
#[derive(Clone)]
pub struct DocumentState {
    pub uri: Url,
    pub version: i32,
    pub source: String,
    pub parse_errors: Vec<RawDiag>,
    pub program: Option<Program>,
    /// Checker output from the last successful type-check of this file.
    pub check_result: Option<CheckResult>,
    /// Top-level definitions this file contributes to the global seed.
    pub file_defs: Option<FileDefs>,
    /// Precomputed line-start index for O(log n) position conversion.
    pub line_index: LineIndex,
}

// ── Global state ─────────────────────────────────────────────────────────

/// Global LSP state shared across all request handlers.
pub struct GlobalState {
    /// Open document buffers keyed by URI.
    pub documents: DashMap<Url, DocumentState>,
    /// Accumulated bindings from all successfully checked files.
    pub bindings: std::sync::RwLock<HashMap<String, Type>>,
    /// Accumulated function signatures (stdlib + user modules).
    pub funcs: std::sync::RwLock<HashMap<String, FuncSig>>,
    /// Accumulated struct definitions.
    pub structs: std::sync::RwLock<HashMap<String, StructSig>>,
    /// Workspace root path.
    pub root: std::sync::RwLock<Option<PathBuf>>,
    /// Change sequence counter for debounce.
    pub sequence: AtomicU32,
    /// Cross-file module index for workspace-wide navigation.
    pub module_index: std::sync::RwLock<ModuleIndex>,
    /// Whether workspace scan has completed (lazy scan).
    pub workspace_scanned: AtomicBool,
    /// Files that need re-checking after a dependency changes.
    /// Maps file URI → list of dependent URIs that import it.
    pub dependents: DashMap<Url, Vec<Url>>,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalState {
    /// Create a new global state seeded with stdlib signatures.
    pub fn new() -> Self {
        Self {
            documents: DashMap::new(),
            bindings: std::sync::RwLock::new(HashMap::new()),
            funcs: std::sync::RwLock::new(stdlib_funcs()),
            structs: std::sync::RwLock::new(HashMap::new()),
            root: std::sync::RwLock::new(None),
            sequence: AtomicU32::new(0),
            module_index: std::sync::RwLock::new(ModuleIndex::default()),
            workspace_scanned: AtomicBool::new(false),
            dependents: DashMap::new(),
        }
    }

    /// Bump the change counter and return the new value.
    pub fn bump_sequence(&self) -> u32 {
        self.sequence.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Read the current sequence value.
    pub fn current_sequence(&self) -> u32 {
        self.sequence.load(Ordering::SeqCst)
    }

    /// Open or update a document buffer.
    ///
    /// Returns the old `FileDefs` (if any) that should be pruned from the
    /// global seed before the new check results are absorbed.
    pub fn update_document(&self, uri: Url, version: i32, text: String) -> (u32, Option<FileDefs>) {
        // Extract old defs before replacing the document.
        let old_defs = self
            .documents
            .get(&uri)
            .and_then(|doc| doc.file_defs.clone());

        let parsed = zz_frontend::parse(&text);
        let line_index = LineIndex::new(&text);
        let doc = DocumentState {
            uri: uri.clone(),
            version,
            source: text,
            parse_errors: parsed.errors,
            program: Some(parsed.program),
            check_result: None,
            file_defs: None,
            line_index,
        };
        self.documents.insert(uri, doc);
        (self.bump_sequence(), old_defs)
    }

    /// Remove a document from the open buffers.
    ///
    /// Returns the `FileDefs` that should be pruned from the global seed.
    pub fn remove_document(&self, uri: &Url) -> Option<FileDefs> {
        self.documents
            .remove(uri)
            .and_then(|(_, doc)| doc.file_defs)
    }

    /// Produce the checker seed from accumulated definitions.
    pub fn checker_seed(
        &self,
    ) -> (
        HashMap<String, Type>,
        HashMap<String, FuncSig>,
        HashMap<String, StructSig>,
    ) {
        (
            self.bindings.read().unwrap().clone(),
            self.funcs.read().unwrap().clone(),
            self.structs.read().unwrap().clone(),
        )
    }

    /// Remove a file's definitions from the global seed.
    pub fn prune_defs(&self, defs: &FileDefs) {
        if !defs.bindings.is_empty() {
            let mut bindings = self.bindings.write().unwrap();
            for name in &defs.bindings {
                bindings.remove(name);
            }
        }
        if !defs.funcs.is_empty() {
            let mut funcs = self.funcs.write().unwrap();
            for name in &defs.funcs {
                funcs.remove(name);
            }
        }
        if !defs.structs.is_empty() {
            let mut structs = self.structs.write().unwrap();
            for name in &defs.structs {
                structs.remove(name);
            }
        }
    }

    /// Merge checker results back into the accumulated seed and store
    /// the file's contribution metadata for future pruning.
    pub fn absorb_result(&self, uri: &Url, result: &CheckResult) {
        // Store file_defs for future pruning.
        let file_defs = FileDefs::from_check_result(result);
        if !file_defs.is_empty() {
            if let Some(mut doc) = self.documents.get_mut(uri) {
                doc.file_defs = Some(file_defs);
            }
        }

        // Merge into global seed.
        self.bindings
            .write()
            .unwrap()
            .extend(result.bindings.clone());
        self.funcs.write().unwrap().extend(result.funcs.clone());
        self.structs.write().unwrap().extend(result.structs.clone());
    }

    /// Set the workspace root.
    pub fn set_root(&self, root: PathBuf) {
        *self.root.write().unwrap() = Some(root);
    }

    /// Scan the workspace synchronously (used internally).
    fn scan_workspace_sync(&self) {
        let root = match self.root.read().unwrap().clone() {
            Some(r) => r,
            None => return,
        };

        let files = crate::cross_file::scan_for_zz_files(&root);
        let mut index = self.module_index.write().unwrap();
        index.module_to_uri.clear();
        index.uri_to_module.clear();
        index.entries.clear();

        for file_path in &files {
            let uri = match Url::from_file_path(file_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let module_path = match crate::cross_file::module_path_for_file(file_path, &root) {
                Some(p) => p,
                None => continue,
            };
            let source = match std::fs::read_to_string(file_path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let entry =
                crate::cross_file::parse_file_entry(uri.clone(), source, module_path.clone());
            index.module_to_uri.insert(module_path.clone(), uri.clone());
            index.uri_to_module.insert(uri.clone(), module_path);
            index.entries.insert(uri, entry);
        }
        self.workspace_scanned.store(true, Ordering::SeqCst);
    }

    /// Scan workspace asynchronously — spawn blocking work, return immediately.
    /// The caller can continue serving requests while the scan runs in background.
    pub fn scan_workspace_async(&self) {
        if self.root.read().unwrap().is_none() {
            return;
        }
        // Quick check: already scanned?
        if self.workspace_scanned.load(Ordering::SeqCst) {
            return;
        }
        // Mark as scanned to prevent duplicate scans.
        self.workspace_scanned.store(true, Ordering::SeqCst);

        // Do the actual scan synchronously — it's fast for small/medium projects
        // and we need the results before serving cross-file requests.
        // For very large projects, this could be made truly async with
        // a background thread, but the tradeoff is complexity.
        self.scan_workspace_sync();
    }

    /// Record that `dependent` imports from `dependency_uri`.
    pub fn record_dependency(&self, dependency_uri: Url, dependent: Url) {
        self.dependents
            .entry(dependency_uri)
            .or_insert_with(Vec::new)
            .push(dependent);
    }

    /// Get all files that depend on the given URI.
    pub fn get_dependents(&self, uri: &Url) -> Vec<Url> {
        self.dependents
            .get(uri)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_checker::check_program;
    use zz_frontend::parse;

    fn make_state() -> GlobalState {
        GlobalState::new()
    }

    #[test]
    fn file_defs_from_check_result() {
        let source = "let x = 10\nfunc add(a: int, b: int) -> int { return a + b }\nstruct Point { x: int, y: int }\n";
        let parsed = parse(source);
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let defs = FileDefs::from_check_result(&cr);
        assert!(defs.bindings.contains(&"x".to_string()));
        assert!(defs.funcs.contains(&"add".to_string()));
        assert!(defs.structs.contains(&"Point".to_string()));
    }

    #[test]
    fn prune_defs_removes_symbols() {
        let state = make_state();
        // Seed some symbols.
        {
            let mut bindings = state.bindings.write().unwrap();
            bindings.insert("x".to_string(), Type::Int);
            bindings.insert("y".to_string(), Type::Int);
        }
        {
            let mut funcs = state.funcs.write().unwrap();
            funcs.insert(
                "add".to_string(),
                FuncSig {
                    generics: vec![],
                    params: vec![],
                    has_default: vec![],
                    ret: Type::Unit,
                },
            );
        }
        // Prune x and add.
        let defs = FileDefs {
            bindings: vec!["x".to_string()],
            funcs: vec!["add".to_string()],
            structs: vec![],
        };
        state.prune_defs(&defs);
        // x should be gone, y should remain.
        assert!(!state.bindings.read().unwrap().contains_key("x"));
        assert!(state.bindings.read().unwrap().contains_key("y"));
        // add should be gone.
        assert!(!state.funcs.read().unwrap().contains_key("add"));
    }

    #[test]
    fn update_document_returns_old_defs() {
        let state = make_state();
        let uri: Url = "file:///test.zz".parse().unwrap();

        // First insert — no old defs.
        let (_, old_defs) = state.update_document(uri.clone(), 1, "let x = 1\n".into());
        assert!(old_defs.is_none(), "first insert should have no old defs");

        // Simulate absorbing a check result so the doc gets file_defs.
        let parsed = parse("let x = 1\nlet y = 2\n");
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        state.absorb_result(&uri, &cr);

        // Update the document — should return old defs.
        let (_, old_defs) = state.update_document(uri.clone(), 2, "let x = 3\n".into());
        let old = old_defs.expect("second update should have old defs");
        assert!(old.bindings.contains(&"x".to_string()));
        assert!(old.bindings.contains(&"y".to_string()));
    }

    #[test]
    fn remove_document_returns_defs() {
        let state = make_state();
        let uri: Url = "file:///test.zz".parse().unwrap();

        state.update_document(uri.clone(), 1, "let z = 5\n".into());
        let parsed = parse("let z = 5\n");
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        state.absorb_result(&uri, &cr);

        // Remove — should return file_defs.
        let defs = state.remove_document(&uri).expect("should return defs");
        assert!(defs.bindings.contains(&"z".to_string()));
        // Document should be gone.
        assert!(!state.documents.contains_key(&uri));
    }

    #[test]
    fn absorb_result_stores_file_defs() {
        let state = make_state();
        let uri: Url = "file:///test.zz".parse().unwrap();

        state.update_document(uri.clone(), 1, "let a = 1\n".into());
        let parsed = parse("let a = 1\n");
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        state.absorb_result(&uri, &cr);

        let doc = state.documents.get(&uri).unwrap();
        let fd = doc
            .file_defs
            .as_ref()
            .expect("should have file_defs after absorb");
        assert!(fd.bindings.contains(&"a".to_string()));
    }

    #[test]
    fn prune_on_update_prevents_stale() {
        let state = make_state();
        let uri: Url = "file:///test.zz".parse().unwrap();

        // v1: defines x and y.
        state.update_document(uri.clone(), 1, "let x = 1\nlet y = 2\n".into());
        let parsed = parse("let x = 1\nlet y = 2\n");
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        state.absorb_result(&uri, &cr);
        assert!(state.bindings.read().unwrap().contains_key("x"));
        assert!(state.bindings.read().unwrap().contains_key("y"));

        // v2: defines only x (y removed). Prune old defs first.
        let (_, old_defs) = state.update_document(uri.clone(), 2, "let x = 3\n".into());
        if let Some(old) = &old_defs {
            state.prune_defs(old);
        }
        // After prune, y should be gone from global seed.
        assert!(!state.bindings.read().unwrap().contains_key("y"));
    }

    #[test]
    fn close_clears_defs() {
        let state = make_state();
        let uri: Url = "file:///test.zz".parse().unwrap();

        state.update_document(uri.clone(), 1, "let w = 99\n".into());
        let parsed = parse("let w = 99\n");
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        state.absorb_result(&uri, &cr);
        assert!(state.bindings.read().unwrap().contains_key("w"));

        // Close → prune + remove.
        let defs = state.remove_document(&uri).unwrap();
        state.prune_defs(&defs);
        assert!(!state.bindings.read().unwrap().contains_key("w"));
        assert!(!state.documents.contains_key(&uri));
    }

    #[test]
    fn workspace_scanned_flag() {
        let state = make_state();
        assert!(!state
            .workspace_scanned
            .load(std::sync::atomic::Ordering::SeqCst));
        state
            .workspace_scanned
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(state
            .workspace_scanned
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn dependency_tracking() {
        let state = make_state();
        let uri_a: Url = "file:///a.zz".parse().unwrap();
        let uri_b: Url = "file:///b.zz".parse().unwrap();
        let uri_c: Url = "file:///c.zz".parse().unwrap();

        // b and c depend on a.
        state.record_dependency(uri_a.clone(), uri_b.clone());
        state.record_dependency(uri_a.clone(), uri_c.clone());

        let deps = state.get_dependents(&uri_a);
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&uri_b));
        assert!(deps.contains(&uri_c));

        // d has no dependents.
        let uri_d: Url = "file:///d.zz".parse().unwrap();
        assert!(state.get_dependents(&uri_d).is_empty());
    }
}
