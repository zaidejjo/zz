use std::rc::Rc;
use std::sync::Arc;

use zz_frontend::ast::Param;
use zz_frontend::span::Span;

use crate::value::Value;

use super::op::Op;

/// A compiled chunk of bytecode: instructions plus a constant pool.
///
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Process-unique identity, used by per-op caches (a pointer would be
    /// reused after the chunk drops). Opaque: only compared for equality
    /// within one interpreter.
    /// Excluded from `PartialEq` (see the manual impl below).
    pub id: u64,
    pub code: Vec<Op>,
    pub constants: Vec<Value>,
    /// File label this chunk was compiled from (`None` = unattributed:
    /// stdlib, `.zzc` loads, REPL snippets). Stamped post-compile by
    /// `Interp::register_coverage_chunk`; see `crate::coverage`.
    pub source: Option<Arc<str>>,
    /// Dense coverage id per opcode, parallel to `code`/`spans`
    /// (`COV_UNMAPPED` = skip). Filled by `register_coverage_chunk`.
    pub cov_ids: Vec<u32>,
    /// Branch id per opcode for conditional jumps, parallel to `code`
    /// (`COV_UNMAPPED` = not a branch). Same fill path as `cov_ids`.
    pub cov_branch: Vec<u32>,
    /// Parameter list when this chunk is a function body (empty otherwise).
    pub params: Vec<Param>,
    /// Source span for each opcode. Same length as `code`.
    /// `Span::default()` for ops that don't carry span info.
    pub spans: Vec<Span>,
    /// Top-level vars promoted to frame slots: `(env_name, slot_index)`.
    /// The VM syncs these values into the environment at frame exit so
    /// later chunks (REPL statements, other modules) can still read them.
    pub toplevel_slots: Vec<(String, u16)>,
    /// Declared type per frame slot, recorded at typed compile time from
    /// named bindings (lets, reassignments, loop variables) via the
    /// checker's expression types. `None` (or short length) means unknown:
    /// temps, untyped compiles, and anything the recorder skipped.
    /// Entries join on reuse (conflicts become `Error`, the "unknown"
    /// top), so every entry over-approximates all values its slot ever
    /// holds. Lowering carries this into the IR locals table;
    /// behavior-neutral (never read by the VM).
    pub slot_types: Vec<Option<zz_checker::Type>>,
    /// Loop-variable frame slots per `ForNext` op, in chunk order (kth
    /// entry ↔ kth `ForNext`): the slot each pushed loop value lands in
    /// (`u16::MAX` for env-captured vars, which bind in the environment).
    /// Lowering carries this into the IR vartab; behavior-neutral (the
    /// VM resolves by name). Lets AOT loop peels write the shadow slot
    /// directly instead of re-deriving positions.
    pub fornext_slots: Vec<Vec<u16>>,
}

/// Structural equality, [`Chunk::id`] excluded: `Value`/`Op` derive
/// `PartialEq` and comparing two compiled functions must not start
/// depending on which one was built first.
impl PartialEq for Chunk {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
            && self.constants == other.constants
            && self.source == other.source
            && self.cov_ids == other.cov_ids
            && self.cov_branch == other.cov_branch
            && self.params == other.params
            && self.spans == other.spans
            && self.toplevel_slots == other.toplevel_slots
            && self.slot_types == other.slot_types
            && self.fornext_slots == other.fornext_slots
    }
}

impl Chunk {
    pub fn new() -> Self {
        static NEXT_CHUNK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Chunk {
            id: NEXT_CHUNK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            code: Vec::new(),
            constants: Vec::new(),
            source: None,
            cov_ids: Vec::new(),
            cov_branch: Vec::new(),
            params: Vec::new(),
            spans: Vec::new(),
            toplevel_slots: Vec::new(),
            slot_types: Vec::new(),
            fornext_slots: Vec::new(),
        }
    }

    /// Convert an `Rc<Chunk>` into an `Arc<Chunk>` suitable for sending
    /// across thread boundaries (e.g. for `spawn`).  Tries `try_unwrap`
    /// first (zero-copy when refcount is 1); falls back to cloning.
    pub fn into_arc(rc: Rc<Chunk>) -> Arc<Chunk> {
        match Rc::try_unwrap(rc) {
            Ok(chunk) => Arc::new(chunk),
            Err(rc) => Arc::new((*rc).clone()),
        }
    }
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}
