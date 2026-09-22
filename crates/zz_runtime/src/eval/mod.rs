mod tree_walker;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use zz_frontend::ast::Program;
use zz_frontend::span::Span;

use crate::env::Env;
use crate::runtime::Flow;
use crate::value::{FuncValue, Value};

pub use crate::runtime::{EvalError, NativeEntry, NativeFn, RuntimeState};

pub struct Interp {
    pub env: Rc<RefCell<Env>>,
    pub funcs: HashMap<String, FuncValue>,
    pub natives: HashMap<String, NativeEntry>,
    pub structs: HashMap<String, Vec<String>>,
    pub args: Vec<String>,
    pub defer_stacks: Vec<Vec<Value>>,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives: HashMap::new(),
            structs: HashMap::new(),
            args: Vec::new(),
            defer_stacks: Vec::new(),
        }
    }

    pub fn with_natives(natives: HashMap<String, NativeEntry>) -> Self {
        Interp {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives,
            structs: HashMap::new(),
            args: Vec::new(),
            defer_stacks: Vec::new(),
        }
    }

    pub fn run(&mut self, program: &Program) -> Result<Value, EvalError> {
        let chunk = Rc::new(crate::vm::Compiler::compile_program(program));
        let mut vm = crate::vm::Vm::new();
        match vm.run_chunk(&chunk, self)? {
            Flow::Value(v) => Ok(v),
            Flow::Return(_) => Err(EvalError::new(
                "`return` outside of a function",
                Span::new(0, 0),
            )),
            Flow::Break => Err(EvalError::new("`break` outside of a loop", Span::new(0, 0))),
            Flow::Continue => Err(EvalError::new(
                "`continue` outside of a loop",
                Span::new(0, 0),
            )),
        }
    }

    pub fn run_tree_walker(&mut self, program: &Program) -> Result<Value, EvalError> {
        let mut result = Value::Unit;
        for stmt in &program.stmts {
            result = self.run_stmt(stmt)?.into_value()?;
        }
        Ok(result)
    }
}
