use std::rc::Rc;
use std::sync::Arc;

use zz_frontend::ast::Param;
use zz_frontend::span::Span;

use crate::value::Value;

use super::op::Op;

/// A compiled chunk of bytecode: instructions plus a constant pool.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub code: Vec<Op>,
    pub constants: Vec<Value>,
    /// Parameter list when this chunk is a function body (empty otherwise).
    pub params: Vec<Param>,
    /// Source span for each opcode. Same length as `code`.
    /// `Span::default()` for ops that don't carry span info.
    pub spans: Vec<Span>,
    /// Top-level vars promoted to frame slots: `(env_name, slot_index)`.
    /// The VM syncs these values into the environment at frame exit so
    /// later chunks (REPL statements, other modules) can still read them.
    pub toplevel_slots: Vec<(String, u16)>,
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            code: Vec::new(),
            constants: Vec::new(),
            params: Vec::new(),
            spans: Vec::new(),
            toplevel_slots: Vec::new(),
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
