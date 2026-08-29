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
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            code: Vec::new(),
            constants: Vec::new(),
            params: Vec::new(),
            spans: Vec::new(),
        }
    }
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}
