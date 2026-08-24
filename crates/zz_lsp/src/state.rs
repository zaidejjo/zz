//! In-memory document buffers and accumulated checker context.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use dashmap::DashMap;
use tower_lsp::lsp_types::Url;
use zz_checker::{CheckResult, FuncSig, StructSig, Type};
use zz_frontend::ast::Program;
use zz_frontend::diag::RawDiag;
use zz_stdlib::stdlib_funcs;

/// Per-file state held in memory.
#[derive(Clone)]
pub struct DocumentState {
    pub uri: Url,
    pub version: i32,
    pub source: String,
    pub parse_errors: Vec<RawDiag>,
    pub program: Option<Program>,
}

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

    /// Open or update a document buffer. Returns the new change sequence.
    pub fn update_document(&self, uri: Url, version: i32, text: String) -> u32 {
        let parsed = zz_frontend::parse(&text);
        let doc = DocumentState {
            uri: uri.clone(),
            version,
            source: text,
            parse_errors: parsed.errors,
            program: Some(parsed.program),
        };
        self.documents.insert(uri, doc);
        self.bump_sequence()
    }

    /// Remove a document from the open buffers.
    pub fn remove_document(&self, uri: &Url) {
        self.documents.remove(uri);
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

    /// Merge checker results back into the accumulated seed.
    pub fn absorb_result(&self, result: &CheckResult) {
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
}
