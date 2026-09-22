use zz_frontend::ast::Param;

use crate::value::Value;

use super::op::Op;

/// A compiled chunk of bytecode: instructions plus a constant pool.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub code: Vec<Op>,
    pub constants: Vec<Value>,
    /// Parameter list when this chunk is a function body (empty otherwise).
    pub params: Vec<Param>,
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            code: Vec::new(),
            constants: Vec::new(),
            params: Vec::new(),
        }
    }
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}
