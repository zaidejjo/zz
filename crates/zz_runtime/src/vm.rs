//! Phase 6: bytecode compiler and stack-based virtual machine.
//!
//! The compiler lowers the AST into a flat sequence of [`Op`] instructions
//! stored in a [`Chunk`]. The [`Vm`] executes chunks on a shared value stack
//! with an explicit call-frame stack, so function calls do not recurse in
//! Rust.
//!
//! Native bytecode covers: literals, variables, paths, arithmetic, logical
//! operators, calls (incl. method resolution), `if`, blocks, fmt strings,
//! declarations, assignments (incl. index/field write-back), `return`,
//! functions, structs, `for`/`while` loops with `break`/`continue`, arrays,
//! dicts, indexing, slicing, and ranges.
//!
//! Constructs the compiler does not yet lower are emitted as
//! [`Op::EvalTree`] / [`Op::EvalTreeStmt`], which fall back to the Phase 1
//! tree-walker (closures, `match`, `if let`, `?`, variants). The two engines
//! interoperate freely: a compiled function body may call a tree-walked
//! closure and vice versa, and control flow (`return`, `break`, `continue`)
//! unwinds across the boundary correctly.

use std::cell::RefCell;
use std::rc::Rc;

use zz_frontend::ast::{BinOp, Block, Expr, FmtPart, Param, Program, Stmt, UnOp};
use zz_frontend::span::Span;

use crate::env::Env;
use crate::interp::{EvalError, Flow, Interp};
use crate::value::{FuncValue, NativeFunc, Value};

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

/// Bytecode instructions.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // ---- stack ----
    /// Push a constant from the chunk's constant pool.
    PushConst(u32),
    /// Discard the top of the stack.
    Pop,
    /// Replace the top of the stack with `Bool(v.is_truthy())`.
    Truthy,

    // ---- variables ----
    /// Push the value bound to `name` (env, then funcs, then natives).
    LoadVar(String, Span),
    /// Push the value of a dotted path (direct binding, func, native, or
    /// struct-field walk).
    LoadPath(Vec<String>, Span),
    /// Pop a value, bind it to `name` in the current scope, and push it back
    /// (a declaration evaluates to its value).
    DefineVar(String),
    /// Pop a value and assign it to `name` (walks the scope chain).
    StoreVar(String, Span),
    /// Pop a value and assign it to a dotted path (direct binding or
    /// struct-field walk with write-back).
    StorePath(Vec<String>, Span),

    // ---- functions & structs ----
    /// Create a named function value from a pre-compiled body chunk, register
    /// it in `funcs` and the current scope, and push unit.
    MakeFunc {
        name: String,
        params: Vec<Param>,
        chunk: Rc<Chunk>,
    },
    /// Register a struct definition (name -> ordered field names).
    RegisterStruct { name: String, fields: Vec<String> },

    // ---- arithmetic ----
    /// Pop `b`, pop `a`, push `a op b` (int/float/str semantics).
    BinOp(BinOp, Span),
    /// Pop `v`, push `op v`.
    UnOp(UnOp, Span),

    // ---- control flow ----
    /// Unconditional jump to an absolute instruction index.
    Jump(usize),
    /// Pop a value; jump if it is falsy.
    JumpIfFalse(usize),
    /// Pop a value; jump if it is truthy.
    JumpIfTrue(usize),
    /// Pop a value; error unless it is a bool; jump if false.
    JumpIfFalseBool(usize, Span),
    /// Pop a value and return it from the current function frame.
    Return,

    // ---- loops ----
    /// Pop an iterable (array or range), push it back plus an index counter,
    /// and push a loop frame. `exit`/`header` are patched by the compiler.
    ForSetup {
        exit: usize,
        header: usize,
        span: Span,
    },
    /// Advance a `for` loop: pop the index, push `index + 1` and the current
    /// item (bound to `var` in a fresh iteration scope), or exit when the
    /// iterable is exhausted.
    ForNext { var: String, exit: usize },
    /// Push a `while` loop frame. `exit`/`header` are patched by the
    /// compiler.
    WhileSetup { exit: usize, header: usize },
    /// Pop the condition; error unless it is a bool; exit the loop when
    /// falsy.
    WhileCond { exit: usize, span: Span },
    /// Exit the innermost loop (restores the loop's environment).
    Break,
    /// Jump to the innermost loop's header (restores the loop's environment).
    Continue,
    /// Pop a value and store it as the innermost loop's result.
    SetLoopResult,

    // ---- collections ----
    /// Pop `n` values and push an array.
    MakeArray(u16),
    /// Pop `2n` values (key, value pairs) and push a dict.
    MakeDict(u16),
    /// Pop an index, pop an object, push `object[index]`.
    IndexOp(Span),
    /// Pop a value, an index, and an object; write `object[index] = value`;
    /// push the mutated object back (for write-back).
    StoreIndexOp(Span),
    /// Pop an end bound, a start bound (either `int` or `Unit` for absent),
    /// and an object; push the slice.
    SliceOp(Span),
    /// Pop an end, pop a start, push a range (both must be ints).
    MakeRange(Span),

    // ---- structs ----
    /// Pop `field_names.len()` values and build a struct instance, matching
    /// them to the registered field order by name.
    MakeStruct {
        name: String,
        field_names: Vec<String>,
        span: Span,
    },
    /// Pop an object, push `object.field`.
    GetField(String, Span),
    /// Pop a value and an object; write `object.field = value`; push the
    /// mutated object back (for write-back).
    SetField(String, Span),

    // ---- calls ----
    /// Pop `argc` arguments, pop the callee, call it, push the result.
    Call { argc: u16, span: Span },
    /// Like `Call` but the callee is a dotted path: resolves method calls
    /// (`p.dist()`) with receiver-first semantics.
    CallPath {
        parts: Vec<String>,
        argc: u16,
        span: Span,
        pspan: Span,
    },

    // ---- fmt ----
    /// Pop `n` values, concatenate their Display forms, push the string.
    Concat(u16),

    // ---- scopes ----
    /// Enter a new child scope.
    EnterScope,
    /// Leave the current scope, restoring its parent.
    ExitScope,

    // ---- tree-walker fallback ----
    /// Evaluate an expression with the tree-walker and push its value.
    EvalTree(Expr),
    /// Run a statement with the tree-walker and push its value.
    EvalTreeStmt(Stmt),
}

/// Lowers an AST into a [`Chunk`].
pub struct Compiler {
    chunk: Chunk,
}

enum JumpKind {
    Always,
    IfFalse,
    IfTrue,
    IfFalseBool(Span),
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Compiler {
    pub fn new() -> Self {
        Compiler {
            chunk: Chunk::new(),
        }
    }

    /// Compile a whole program. The top level runs in the interpreter's root
    /// scope (no `EnterScope`), matching the tree-walker.
    pub fn compile_program(program: &Program) -> Chunk {
        let mut c = Compiler::new();
        for (i, stmt) in program.stmts.iter().enumerate() {
            c.compile_stmt(stmt);
            if i < program.stmts.len() - 1 {
                c.emit(Op::Pop);
            }
        }
        if program.stmts.is_empty() {
            c.emit_const(Value::Unit);
        }
        c.chunk
    }

    fn emit(&mut self, op: Op) {
        self.chunk.code.push(op);
    }

    fn push_const(&mut self, v: Value) -> u32 {
        self.chunk.constants.push(v);
        (self.chunk.constants.len() - 1) as u32
    }

    fn emit_const(&mut self, v: Value) {
        let idx = self.push_const(v);
        self.emit(Op::PushConst(idx));
    }

    fn emit_jump(&mut self, kind: JumpKind) -> usize {
        let pos = self.chunk.code.len();
        let op = match kind {
            JumpKind::Always => Op::Jump(0),
            JumpKind::IfFalse => Op::JumpIfFalse(0),
            JumpKind::IfTrue => Op::JumpIfTrue(0),
            JumpKind::IfFalseBool(span) => Op::JumpIfFalseBool(0, span),
        };
        self.chunk.code.push(op);
        pos
    }

    fn patch_jump(&mut self, pos: usize) {
        let target = self.chunk.code.len();
        let op = std::mem::replace(&mut self.chunk.code[pos], Op::Jump(0));
        self.chunk.code[pos] = match op {
            Op::Jump(_) => Op::Jump(target),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(target),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(target),
            Op::JumpIfFalseBool(_, span) => Op::JumpIfFalseBool(target, span),
            Op::ForNext { var, .. } => Op::ForNext { var, exit: target },
            Op::WhileCond { span, .. } => Op::WhileCond { exit: target, span },
            other => panic!("patch_jump on non-jump op: {other:?}"),
        };
    }

    fn emit_for_next(&mut self, var: &str) -> usize {
        let pos = self.chunk.code.len();
        self.emit(Op::ForNext {
            var: var.to_string(),
            exit: 0,
        });
        pos
    }

    fn emit_while_cond(&mut self, span: Span) -> usize {
        let pos = self.chunk.code.len();
        self.emit(Op::WhileCond { exit: 0, span });
        pos
    }

    /// Emit the write-back for a mutated object: `arr[0] = v` stores the
    /// mutated array back into its variable / struct field chain.
    fn compile_write_back(&mut self, target: &Expr) {
        match target {
            Expr::Ident { name, span } => self.emit(Op::StoreVar(name.clone(), *span)),
            Expr::Path { parts, span } => self.emit(Op::StorePath(parts.clone(), *span)),
            Expr::Field { obj, name, span } => {
                self.compile_expr(obj);
                self.emit(Op::SetField(name.clone(), *span));
                self.compile_write_back(obj);
            }
            _ => self.emit(Op::Pop),
        }
    }

    fn compile_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                self.compile_expr(value);
                self.emit(Op::DefineVar(name.name.clone()));
            }
            Stmt::Import { .. } => {
                // Imports are resolved by the loader/session; no runtime effect.
            }
            Stmt::Func {
                name, params, body, ..
            } => {
                let chunk = self.compile_func_body(body, params);
                self.emit(Op::MakeFunc {
                    name: name.name.clone(),
                    params: params.clone(),
                    chunk,
                });
            }
            Stmt::Return { value, .. } => {
                match value {
                    Some(e) => self.compile_expr(e),
                    None => self.emit_const(Value::Unit),
                }
                self.emit(Op::Return);
            }
            Stmt::Struct { name, fields, .. } => {
                self.emit(Op::RegisterStruct {
                    name: name.name.clone(),
                    fields: fields.iter().map(|(n, _)| n.name.clone()).collect(),
                });
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => {
                // Result slot (last body value), then the iterable, then the
                // index counter. The loop frame records the stack layout so
                // `break`/`continue` can unwind precisely.
                self.emit_const(Value::Unit);
                self.compile_expr(iter);
                let setup_pos = self.chunk.code.len();
                self.emit(Op::ForSetup {
                    exit: 0,
                    header: 0,
                    span: *span,
                });
                let header = self.chunk.code.len();
                let j = self.emit_for_next(&var.name);
                self.compile_block_body(body);
                self.emit(Op::SetLoopResult);
                self.emit(Op::Jump(header));
                self.patch_jump(j);
                let exit = self.chunk.code.len();
                self.chunk.code[setup_pos] = Op::ForSetup {
                    exit,
                    header,
                    span: *span,
                };
            }
            Stmt::Break { .. } => self.emit(Op::Break),
            Stmt::Continue { .. } => self.emit(Op::Continue),
            Stmt::Assign { target, value, .. } => match target {
                Expr::Ident { name, span } => {
                    self.compile_expr(value);
                    self.emit(Op::StoreVar(name.clone(), *span));
                    self.emit_const(Value::Unit);
                }
                Expr::Path { parts, span } => {
                    self.compile_expr(value);
                    self.emit(Op::StorePath(parts.clone(), *span));
                    self.emit_const(Value::Unit);
                }
                Expr::Index { obj, index, span } => {
                    self.compile_expr(value);
                    self.compile_expr(index);
                    self.compile_expr(obj);
                    self.emit(Op::StoreIndexOp(*span));
                    self.compile_write_back(obj);
                    self.emit_const(Value::Unit);
                }
                Expr::Field { obj, name, span } => {
                    self.compile_expr(value);
                    self.compile_expr(obj);
                    self.emit(Op::SetField(name.clone(), *span));
                    // The tree-walker writes a mutated object back only when
                    // the base is a plain variable.
                    if let Expr::Ident { name, .. } = &**obj {
                        self.emit(Op::StoreVar(name.clone(), *span));
                    } else {
                        self.emit(Op::Pop);
                    }
                    self.emit_const(Value::Unit);
                }
                _ => self.emit(Op::EvalTreeStmt(stmt.clone())),
            },
            Stmt::Expr(e) => self.compile_expr(e),
        }
    }

    /// Compile a block: child scope, statements (non-final values popped),
    /// final value left on the stack, scope exit.
    fn compile_block(&mut self, block: &Block) {
        self.emit(Op::EnterScope);
        self.compile_block_body(block);
        self.emit(Op::ExitScope);
    }

    /// Compile a block's statements without a surrounding scope. Used for
    /// loop bodies, which already run in a fresh iteration scope.
    fn compile_block_body(&mut self, block: &Block) {
        for (i, stmt) in block.stmts.iter().enumerate() {
            self.compile_stmt(stmt);
            if i < block.stmts.len() - 1 {
                self.emit(Op::Pop);
            }
        }
        if block.stmts.is_empty() {
            self.emit_const(Value::Unit);
        }
    }

    /// Compile a function body into its own chunk. The parameter list is
    /// stored on the chunk so `call_func` can arity-check it.
    fn compile_func_body(&mut self, block: &Block, params: &[Param]) -> Rc<Chunk> {
        let mut sub = Compiler::new();
        sub.chunk.params = params.to_vec();
        sub.compile_block(block);
        Rc::new(sub.chunk)
    }

    fn compile_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { value, .. } => self.emit_const(Value::Int(*value)),
            Expr::Float { value, .. } => self.emit_const(Value::Float(*value)),
            Expr::Str { value, .. } => self.emit_const(Value::Str(value.clone())),
            Expr::Bool { value, .. } => self.emit_const(Value::Bool(*value)),
            Expr::Ident { name, span } => self.emit(Op::LoadVar(name.clone(), *span)),
            Expr::Path { parts, span } => self.emit(Op::LoadPath(parts.clone(), *span)),
            Expr::Paren { expr, .. } => self.compile_expr(expr),
            Expr::Unary { op, expr, span } => {
                self.compile_expr(expr);
                self.emit(Op::UnOp(*op, *span));
            }
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => match op {
                BinOp::And => {
                    self.compile_expr(left);
                    let j = self.emit_jump(JumpKind::IfFalse);
                    self.compile_expr(right);
                    self.emit(Op::Truthy);
                    let j2 = self.emit_jump(JumpKind::Always);
                    self.patch_jump(j);
                    self.emit_const(Value::Bool(false));
                    self.patch_jump(j2);
                }
                BinOp::Or => {
                    self.compile_expr(left);
                    let j = self.emit_jump(JumpKind::IfTrue);
                    self.compile_expr(right);
                    self.emit(Op::Truthy);
                    let j2 = self.emit_jump(JumpKind::Always);
                    self.patch_jump(j);
                    self.emit_const(Value::Bool(true));
                    self.patch_jump(j2);
                }
                _ => {
                    self.compile_expr(left);
                    self.compile_expr(right);
                    self.emit(Op::BinOp(*op, *span));
                }
            },
            Expr::Call { callee, args, span } => match callee.as_ref() {
                Expr::Path { parts, span: pspan } => {
                    for a in args {
                        self.compile_expr(a);
                    }
                    self.emit(Op::CallPath {
                        parts: parts.clone(),
                        argc: args.len() as u16,
                        span: *span,
                        pspan: *pspan,
                    });
                }
                _ => {
                    self.compile_expr(callee);
                    for a in args {
                        self.compile_expr(a);
                    }
                    self.emit(Op::Call {
                        argc: args.len() as u16,
                        span: *span,
                    });
                }
            },
            Expr::If {
                cond,
                then,
                els,
                span,
            } => {
                self.compile_expr(cond);
                let j = self.emit_jump(JumpKind::IfFalseBool(*span));
                self.compile_block(then);
                let j2 = self.emit_jump(JumpKind::Always);
                self.patch_jump(j);
                match els {
                    Some(e) => self.compile_expr(e),
                    None => self.emit_const(Value::Unit),
                }
                self.patch_jump(j2);
            }
            Expr::Block(b) => self.compile_block(b),
            Expr::Fmt { parts, .. } => {
                let mut n = 0u16;
                for part in parts {
                    match part {
                        FmtPart::Text(t) => {
                            self.emit_const(Value::Str(t.clone()));
                            n += 1;
                        }
                        FmtPart::Expr(e) => {
                            self.compile_expr(e);
                            n += 1;
                        }
                    }
                }
                self.emit(Op::Concat(n));
            }
            Expr::While { cond, body, span } => {
                let setup_pos = self.chunk.code.len();
                self.emit(Op::WhileSetup { exit: 0, header: 0 });
                self.emit_const(Value::Unit);
                let header = self.chunk.code.len();
                self.compile_expr(cond);
                let j = self.emit_while_cond(*span);
                self.compile_block_body(body);
                self.emit(Op::SetLoopResult);
                self.emit(Op::Jump(header));
                self.patch_jump(j);
                let exit = self.chunk.code.len();
                self.chunk.code[setup_pos] = Op::WhileSetup { exit, header };
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.compile_expr(e);
                }
                self.emit(Op::MakeArray(elems.len() as u16));
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    self.compile_expr(k);
                    self.compile_expr(v);
                }
                self.emit(Op::MakeDict(entries.len() as u16));
            }
            Expr::Field { obj, name, span } => {
                self.compile_expr(obj);
                self.emit(Op::GetField(name.clone(), *span));
            }
            Expr::Range { start, end, span } => {
                self.compile_expr(start);
                self.compile_expr(end);
                self.emit(Op::MakeRange(*span));
            }
            Expr::StructInit { name, fields, span } => {
                for (_, v) in fields {
                    self.compile_expr(v);
                }
                self.emit(Op::MakeStruct {
                    name: name.clone(),
                    field_names: fields.iter().map(|(n, _)| n.clone()).collect(),
                    span: *span,
                });
            }
            Expr::Index { obj, index, span } => {
                self.compile_expr(obj);
                self.compile_expr(index);
                self.emit(Op::IndexOp(*span));
            }
            Expr::Slice {
                obj,
                start,
                end,
                span,
            } => {
                self.compile_expr(obj);
                match start {
                    Some(e) => self.compile_expr(e),
                    None => self.emit_const(Value::Unit),
                }
                match end {
                    Some(e) => self.compile_expr(e),
                    None => self.emit_const(Value::Unit),
                }
                self.emit(Op::SliceOp(*span));
            }
            // Everything not yet lowered runs through the tree-walker.
            other => self.emit(Op::EvalTree(other.clone())),
        }
    }
}

/// One active call frame.
struct Frame {
    chunk: Rc<Chunk>,
    ip: usize,
    /// Environment to restore when this frame returns.
    prev_env: Rc<RefCell<Env>>,
    /// Stack index where this frame's evaluation begins.
    stack_base: usize,
}

/// One active loop (native `for`/`while`). Used by `break`/`continue` to
/// unwind the stack and restore the environment.
struct LoopInfo {
    /// Jump target for `break` / loop exit.
    exit: usize,
    /// Jump target for `continue` (the loop header).
    header: usize,
    /// Environment at loop start; iteration scopes are children of it.
    env: Rc<RefCell<Env>>,
    /// Frame index that pushed this loop, so `break` inside a function body
    /// cannot capture a caller's loop.
    frame_idx: usize,
    /// Stack slot of the loop's result value.
    stack_base: usize,
    /// Extra slots pushed above the result (iterable + index for `for`).
    slots: usize,
}

/// Result of unwinding a frame after a control-flow signal.
enum Unwind {
    /// Frame unwound and the value was pushed onto the caller's stack.
    Continue,
    /// The program frame was unwound: propagate the flow to `run_chunk`'s
    /// caller.
    Escaped(Flow),
    /// A `break`/`continue` escaped a function body: error at the call site.
    Error(EvalError),
}

/// A stack-based virtual machine. Executes compiled chunks against an
/// [`Interp`], sharing its environment, function table, and native registry.
pub struct Vm {
    stack: Vec<Value>,
    frames: Vec<Frame>,
    loops: Vec<LoopInfo>,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    pub fn new() -> Self {
        Vm {
            stack: Vec::new(),
            frames: Vec::new(),
            loops: Vec::new(),
        }
    }

    /// Run a chunk to completion. Returns the chunk's value, or a control
    /// flow signal (`Return`/`Break`/`Continue`) that escaped the program
    /// frame.
    pub(crate) fn run_chunk(
        &mut self,
        chunk: &Rc<Chunk>,
        interp: &mut Interp,
    ) -> Result<Flow, EvalError> {
        self.frames.push(Frame {
            chunk: Rc::clone(chunk),
            ip: 0,
            prev_env: Rc::clone(&interp.env),
            stack_base: self.stack.len(),
        });

        loop {
            // Fetch the current instruction without holding a borrow across
            // the dispatch (arms may push/pop frames). The chunk is behind an
            // `Rc` and never mutated after compilation, so a raw pointer into
            // its code vector is sound and avoids an atomic refcount bump per
            // instruction.
            let (code, constants, ip) = {
                let f = self.frames.last().unwrap();
                let chunk = unsafe { &*Rc::as_ptr(&f.chunk) };
                (&chunk.code, &chunk.constants, f.ip)
            };

            if ip >= code.len() {
                // Chunk ran off the end: implicit return of the block value.
                let v = if self.stack.len() > self.frames.last().unwrap().stack_base {
                    self.stack.pop().unwrap()
                } else {
                    Value::Unit
                };
                let f = self.frames.pop().unwrap();
                self.stack.truncate(f.stack_base);
                interp.env = f.prev_env;
                if self.frames.is_empty() {
                    return Ok(Flow::Value(v));
                }
                self.stack.push(v);
                continue;
            }

            let op = &code[ip];
            self.frames.last_mut().unwrap().ip = ip + 1;

            match op {
                Op::PushConst(i) => {
                    let v = constants[*i as usize].clone();
                    self.stack.push(v);
                }
                Op::Pop => {
                    self.stack.pop();
                }
                Op::Truthy => {
                    let v = self.stack.pop().unwrap();
                    self.stack.push(Value::Bool(v.is_truthy()));
                }
                Op::LoadVar(name, span) => {
                    let v = interp
                        .env
                        .borrow()
                        .get(name)
                        .or_else(|| interp.funcs.get(name).map(|fv| Value::Func(fv.clone())))
                        .or_else(|| {
                            interp.natives.get(name).map(|entry| {
                                Value::Native(NativeFunc {
                                    name: name.clone(),
                                    arity: entry.arity,
                                })
                            })
                        })
                        .ok_or_else(|| {
                            EvalError::new(format!("undefined variable `{name}`"), *span)
                        })?;
                    self.stack.push(v);
                }
                Op::LoadPath(parts, span) => {
                    let v = interp.resolve_path_value(parts, *span)?;
                    self.stack.push(v);
                }
                Op::DefineVar(name) => {
                    let v = self.stack.pop().unwrap();
                    interp.env.borrow_mut().define(name, v.clone());
                    self.stack.push(v);
                }
                Op::StoreVar(name, span) => {
                    let v = self.stack.pop().unwrap();
                    if !interp.env.borrow_mut().assign(name, v) {
                        return Err(EvalError::new(
                            format!("undefined variable `{name}`"),
                            *span,
                        ));
                    }
                }
                Op::StorePath(parts, span) => {
                    let v = self.stack.pop().unwrap();
                    interp.assign_path(parts, v, *span)?;
                }
                Op::MakeFunc {
                    name,
                    params,
                    chunk: fchunk,
                } => {
                    let fv = FuncValue {
                        params: params.clone(),
                        body: Expr::Block(Block {
                            stmts: Vec::new(),
                            span: Span::new(0, 0),
                        }),
                        env: Rc::clone(&interp.env),
                        chunk: Some(Rc::clone(fchunk)),
                    };
                    interp.funcs.insert(name.clone(), fv.clone());
                    interp.env.borrow_mut().define(name, Value::Func(fv));
                    self.stack.push(Value::Unit);
                }
                Op::RegisterStruct { name, fields } => {
                    interp.structs.insert(name.clone(), fields.clone());
                    self.stack.push(Value::Unit);
                }
                Op::BinOp(op, span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    let v = interp.eval_binary(*op, l, r, *span)?;
                    self.stack.push(v);
                }
                Op::UnOp(op, span) => {
                    let v = self.stack.pop().unwrap();
                    let v = interp.eval_unary(*op, v, *span)?;
                    self.stack.push(v);
                }
                Op::Jump(target) => {
                    self.frames.last_mut().unwrap().ip = *target;
                }
                Op::JumpIfFalse(target) => {
                    let v = self.stack.pop().unwrap();
                    if !v.is_truthy() {
                        self.frames.last_mut().unwrap().ip = *target;
                    }
                }
                Op::JumpIfTrue(target) => {
                    let v = self.stack.pop().unwrap();
                    if v.is_truthy() {
                        self.frames.last_mut().unwrap().ip = *target;
                    }
                }
                Op::JumpIfFalseBool(target, span) => {
                    let v = self.stack.pop().unwrap();
                    if !matches!(v, Value::Bool(_)) {
                        return Err(EvalError::new("`if` condition must be a bool", *span));
                    }
                    if !v.is_truthy() {
                        self.frames.last_mut().unwrap().ip = *target;
                    }
                }
                Op::Return => {
                    let v = self.stack.pop().unwrap();
                    match self.unwind_frame(Flow::Return(v), interp) {
                        Unwind::Continue => {}
                        Unwind::Escaped(flow) => return Ok(flow),
                        Unwind::Error(e) => return Err(e),
                    }
                }
                Op::ForSetup { exit, header, span } => {
                    let it = self.stack.pop().unwrap();
                    let (iterable, idx) = match it.clone() {
                        Value::Array(_) => (it, Value::Int(0)),
                        Value::Range(start, _) => (it, Value::Int(start)),
                        other => {
                            return Err(EvalError::new(
                                format!("cannot iterate a value of type `{other}`"),
                                *span,
                            ))
                        }
                    };
                    let stack_base = self.stack.len() - 1;
                    self.loops.push(LoopInfo {
                        exit: *exit,
                        header: *header,
                        env: Rc::clone(&interp.env),
                        frame_idx: self.frames.len() - 1,
                        stack_base,
                        slots: 2,
                    });
                    self.stack.push(iterable);
                    self.stack.push(idx);
                }
                Op::ForNext { var, exit } => {
                    let idx = self.stack.pop().unwrap();
                    let iterable_idx = self.stack.len() - 1;
                    let (done, item) = match (&self.stack[iterable_idx], &idx) {
                        (Value::Array(items), Value::Int(i)) => {
                            let i = *i;
                            if i >= items.len() as i64 {
                                (true, Value::Unit)
                            } else {
                                (false, items[i as usize].clone())
                            }
                        }
                        (Value::Range(_, end), Value::Int(i)) => {
                            let i = *i;
                            if i >= *end {
                                (true, Value::Unit)
                            } else {
                                (false, Value::Int(i))
                            }
                        }
                        _ => unreachable!("ForNext on non-iterable"),
                    };
                    if done {
                        let li = self.loops.pop().unwrap();
                        self.stack.truncate(li.stack_base + 1);
                        interp.env = li.env;
                        self.frames.last_mut().unwrap().ip = *exit;
                    } else {
                        let next = match idx {
                            Value::Int(i) => Value::Int(i + 1),
                            _ => unreachable!(),
                        };
                        self.stack.push(next);
                        let li = self.loops.last().unwrap();
                        let loop_env = Rc::clone(&li.env);
                        interp.env = loop_env;
                        let scope = Env::with_parent(&interp.env);
                        scope.borrow_mut().define(var, item);
                        interp.env = scope;
                    }
                }
                Op::WhileSetup { exit, header } => {
                    self.loops.push(LoopInfo {
                        exit: *exit,
                        header: *header,
                        env: Rc::clone(&interp.env),
                        frame_idx: self.frames.len() - 1,
                        stack_base: self.stack.len(),
                        slots: 0,
                    });
                }
                Op::WhileCond { exit, span } => {
                    let c = self.stack.pop().unwrap();
                    if !matches!(c, Value::Bool(_)) {
                        return Err(EvalError::new("`while` condition must be a bool", *span));
                    }
                    if !c.is_truthy() {
                        let li = self.loops.pop().unwrap();
                        self.stack.truncate(li.stack_base + 1);
                        interp.env = li.env;
                        self.frames.last_mut().unwrap().ip = *exit;
                    }
                }
                Op::Break => {
                    let Some(li) = self.loops.pop() else {
                        return Err(EvalError::new("`break` outside of a loop", Span::new(0, 0)));
                    };
                    if li.frame_idx != self.frames.len() - 1 {
                        return Err(EvalError::new("`break` outside of a loop", Span::new(0, 0)));
                    }
                    self.stack.truncate(li.stack_base + 1);
                    interp.env = li.env;
                    self.frames.last_mut().unwrap().ip = li.exit;
                }
                Op::Continue => {
                    let Some(li) = self.loops.last() else {
                        return Err(EvalError::new(
                            "`continue` outside of a loop",
                            Span::new(0, 0),
                        ));
                    };
                    if li.frame_idx != self.frames.len() - 1 {
                        return Err(EvalError::new(
                            "`continue` outside of a loop",
                            Span::new(0, 0),
                        ));
                    }
                    self.stack.truncate(li.stack_base + 1 + li.slots);
                    interp.env = Rc::clone(&li.env);
                    self.frames.last_mut().unwrap().ip = li.header;
                }
                Op::SetLoopResult => {
                    let v = self.stack.pop().unwrap();
                    let li = self.loops.last().unwrap();
                    self.stack[li.stack_base] = v;
                }
                Op::MakeArray(n) => {
                    let mut items = Vec::with_capacity(*n as usize);
                    for _ in 0..*n {
                        items.push(self.stack.pop().unwrap());
                    }
                    items.reverse();
                    self.stack.push(Value::Array(items));
                }
                Op::MakeDict(n) => {
                    let mut pairs = Vec::with_capacity(*n as usize);
                    for _ in 0..*n {
                        let v = self.stack.pop().unwrap();
                        let k = self.stack.pop().unwrap();
                        pairs.push((k, v));
                    }
                    pairs.reverse();
                    self.stack.push(Value::Dict(pairs));
                }
                Op::IndexOp(span) => {
                    let iv = self.stack.pop().unwrap();
                    let ov = self.stack.pop().unwrap();
                    let v = Interp::get_index(&ov, &iv, *span)?;
                    self.stack.push(v);
                }
                Op::StoreIndexOp(span) => {
                    let mut ov = self.stack.pop().unwrap();
                    let iv = self.stack.pop().unwrap();
                    let value = self.stack.pop().unwrap();
                    Interp::set_index(&mut ov, &iv, value, *span)?;
                    self.stack.push(ov);
                }
                Op::SliceOp(span) => {
                    let e = self.stack.pop().unwrap();
                    let s = self.stack.pop().unwrap();
                    let ov = self.stack.pop().unwrap();
                    let bound = |v: Value| match v {
                        Value::Int(i) => Ok(Some(i)),
                        Value::Unit => Ok(None),
                        other => Err(EvalError::new(
                            format!("slice bound must be `int`, found `{other}`"),
                            *span,
                        )),
                    };
                    let v = Interp::slice_value(&ov, bound(s)?, bound(e)?, *span)?;
                    self.stack.push(v);
                }
                Op::MakeRange(span) => {
                    let e = self.stack.pop().unwrap();
                    let s = self.stack.pop().unwrap();
                    match (s, e) {
                        (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Range(a, b)),
                        _ => return Err(EvalError::new("range bounds must be integers", *span)),
                    }
                }
                Op::MakeStruct {
                    name,
                    field_names,
                    span,
                } => {
                    let Some(registered) = interp.structs.get(name).cloned() else {
                        return Err(EvalError::new(format!("unknown struct `{name}`"), *span));
                    };
                    let mut vals = Vec::with_capacity(field_names.len());
                    for _ in 0..field_names.len() {
                        vals.push(self.stack.pop().unwrap());
                    }
                    vals.reverse();
                    let mut out = Vec::with_capacity(registered.len());
                    for fname in &registered {
                        let Some(idx) = field_names.iter().position(|n| n == fname) else {
                            return Err(EvalError::new(
                                format!("missing field `{fname}` in struct literal"),
                                *span,
                            ));
                        };
                        out.push((fname.clone(), vals[idx].clone()));
                    }
                    self.stack.push(Value::Object {
                        name: name.clone(),
                        fields: out,
                    });
                }
                Op::GetField(name, span) => {
                    let ov = self.stack.pop().unwrap();
                    let v = Interp::object_field(&ov, name, *span)?;
                    self.stack.push(v);
                }
                Op::SetField(name, span) => {
                    let value = self.stack.pop().unwrap();
                    let mut ov = self.stack.pop().unwrap();
                    Interp::set_object_field(&mut ov, name, value, *span)?;
                    self.stack.push(ov);
                }
                Op::Call { argc, span } => {
                    let argc = *argc;
                    let span = *span;
                    let mut args = Vec::with_capacity(argc as usize);
                    for _ in 0..argc {
                        args.push(self.stack.pop().unwrap());
                    }
                    args.reverse();
                    let callee = self.stack.pop().unwrap();
                    self.call_value(callee, args, span, interp)?;
                }
                Op::CallPath {
                    parts,
                    argc,
                    span,
                    pspan,
                } => {
                    let argc = *argc;
                    let span = *span;
                    let pspan = *pspan;
                    let mut args = Vec::with_capacity(argc as usize);
                    for _ in 0..argc {
                        args.push(self.stack.pop().unwrap());
                    }
                    args.reverse();
                    if parts.len() >= 2 {
                        let joined = parts.join(".");
                        let is_direct = interp.env.borrow().get(&joined).is_some()
                            || interp.funcs.contains_key(&joined)
                            || interp.natives.contains_key(&joined);
                        if !is_direct && interp.resolve_path_value(parts, pspan).is_err() {
                            let method = parts.last().unwrap();
                            let recv =
                                interp.resolve_path_value(&parts[..parts.len() - 1], pspan)?;
                            let f = interp.lookup_method(&recv, method, pspan)?;
                            let mut arg_vals = vec![recv];
                            arg_vals.extend(args);
                            self.call_value(f, arg_vals, span, interp)?;
                            continue;
                        }
                    }
                    let callee = interp.resolve_path_value(parts, pspan)?;
                    self.call_value(callee, args, span, interp)?;
                }
                Op::Concat(n) => {
                    let mut parts = Vec::with_capacity(*n as usize);
                    for _ in 0..*n {
                        parts.push(self.stack.pop().unwrap());
                    }
                    parts.reverse();
                    let mut out = String::new();
                    for p in parts {
                        out.push_str(&p.to_string());
                    }
                    self.stack.push(Value::Str(out));
                }
                Op::EnterScope => {
                    let scope = Env::with_parent(&interp.env);
                    interp.env = scope;
                }
                Op::ExitScope => {
                    let parent = interp
                        .env
                        .borrow()
                        .parent_rc()
                        .expect("ExitScope at top level");
                    interp.env = parent;
                }
                Op::EvalTree(expr) => match interp.eval(expr)? {
                    Flow::Value(v) => self.stack.push(v),
                    flow @ (Flow::Return(_) | Flow::Break | Flow::Continue) => {
                        match self.unwind_frame(flow, interp) {
                            Unwind::Continue => {}
                            Unwind::Escaped(flow) => return Ok(flow),
                            Unwind::Error(e) => return Err(e),
                        }
                    }
                },
                Op::EvalTreeStmt(stmt) => match interp.run_stmt(stmt)? {
                    Flow::Value(v) => self.stack.push(v),
                    flow @ (Flow::Return(_) | Flow::Break | Flow::Continue) => {
                        match self.unwind_frame(flow, interp) {
                            Unwind::Continue => {}
                            Unwind::Escaped(flow) => return Ok(flow),
                            Unwind::Error(e) => return Err(e),
                        }
                    }
                },
            }
        }
    }

    /// Call a value. Chunked functions get an inline frame (no Rust
    /// recursion); everything else goes through `Interp::call` (natives,
    /// tree-walker closures, and arity errors).
    fn call_value(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        span: Span,
        interp: &mut Interp,
    ) -> Result<(), EvalError> {
        match callee {
            Value::Func(fv) if fv.chunk.is_some() => {
                if args.len() != fv.params.len() {
                    return Err(EvalError::new(
                        format!(
                            "expected {} arguments, found {}",
                            fv.params.len(),
                            args.len()
                        ),
                        span,
                    ));
                }
                let scope = Env::with_parent(&fv.env);
                for (p, v) in fv.params.iter().zip(args) {
                    scope.borrow_mut().define(&p.name.name, v);
                }
                let prev_env = std::mem::replace(&mut interp.env, scope);
                self.frames.push(Frame {
                    chunk: fv.chunk.unwrap(),
                    ip: 0,
                    prev_env,
                    stack_base: self.stack.len(),
                });
                Ok(())
            }
            other => {
                let result = interp.call(other, args, span)?;
                self.stack.push(result);
                Ok(())
            }
        }
    }

    /// Pop the current frame after a control-flow signal. `Return` pushes the
    /// value onto the caller's stack; `Break`/`Continue` that escape a
    /// function body are errors (matching the tree-walker's `call_func`).
    fn unwind_frame(&mut self, flow: Flow, interp: &mut Interp) -> Unwind {
        let v = match &flow {
            Flow::Return(v) => v.clone(),
            Flow::Break | Flow::Continue => Value::Unit,
            Flow::Value(_) => unreachable!("unwind_frame on a plain value"),
        };
        let f = self.frames.pop().unwrap();
        self.loops.retain(|li| li.frame_idx < self.frames.len());
        self.stack.truncate(f.stack_base);
        interp.env = f.prev_env;
        if self.frames.is_empty() {
            return Unwind::Escaped(flow);
        }
        match flow {
            Flow::Return(_) => {
                self.stack.push(v);
                Unwind::Continue
            }
            Flow::Break => {
                Unwind::Error(EvalError::new("`break` outside of a loop", Span::new(0, 0)))
            }
            Flow::Continue => Unwind::Error(EvalError::new(
                "`continue` outside of a loop",
                Span::new(0, 0),
            )),
            Flow::Value(_) => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::ast::Ident;
    use zz_frontend::parse;

    fn run_src(src: &str) -> Result<Value, EvalError> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let mut interp = Interp::new();
        interp.run(&parsed.program)
    }

    fn run_tree(src: &str) -> Result<Value, EvalError> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let mut interp = Interp::new();
        interp.run_tree_walker(&parsed.program)
    }

    /// Differential test: the VM and the tree-walker must agree.
    fn assert_same(src: &str) {
        let vm = run_src(src);
        let tree = run_tree(src);
        match (&vm, &tree) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "VM and tree-walker disagree on: {src}"),
            (Err(a), Err(b)) => assert_eq!(
                a.message, b.message,
                "VM and tree-walker disagree on error for: {src}"
            ),
            _ => panic!("VM and tree-walker disagree on: {src}\nVM: {vm:?}\ntree: {tree:?}"),
        }
    }

    #[test]
    fn vm_matches_tree_walker_on_basics() {
        for src in [
            "1 + 2 * 3",
            "(1 + 2) * 3",
            "10 / 3",
            "10 % 3",
            "-5 + 2",
            "1 + 2.5",
            "\"a\" + \"b\"",
            "1 < 2",
            "1 == 1",
            "true && false",
            "true || false",
            "!true",
            "let x = 1 + 2\nx * 3",
            "let a = 10\nlet b = 20\nlet c = a + b\nc",
            "let x = 1\nlet x = x + 1\nx",
            "if true { 1 } else { 2 }",
            "if false { 1 } else { 2 }",
            "if true { 1 }",
            "if false { 1 }",
            "if 1 < 2 { \"yes\" } else { \"no\" }",
            "let name = \"World\"\n\"Hello {name}\"",
            "\"sum: {1 + 2}\"",
            "func dbl(n: int) -> int { n * 2 }\ndbl(21)",
            "func fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(5)",
            "func f() -> int { return 5 }\nf()",
            "func f() -> int { if true { return 5 }\n3 }\nf()",
            "func add(a: int, b: int) -> int { a + b }\nadd(2, 3)",
            "x := 1\nx = 5\nx",
            "x := 1\nx = x + 1\nx",
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x",
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\ndist(Point{ x: 3, y: 4 })",
            "v := [1, 2, 3]\nv[1]",
            "s := \"hello\"\ns[1:3]",
            "m := {1: 2}\nm[1]",
            "x := .some(1)\nmatch x { .some(n) => n, .none => 0 }",
            "x := .none\nmatch x { .some(n) => n, .none => 0 }",
            "f := |x| x * 2\nf(5)",
            "sum := 0\nfor i in 0..5 { sum = sum + i }\nsum",
            "total := 0\nfor n in [10, 20, 30] { total = total + n }\ntotal",
            "found := 0\nfor i in 0..10 { if i == 3 { found = i; break } }\nfound",
            "count := 0\nfor i in 0..5 { if i == 2 { continue }; count = count + 1 }\ncount",
            "x := 1\nif x > 0 { x = 5 }\nx",
            "a := 1\nb := 2\nc := a + b\nc",
            "func even(n: int) -> bool { if n == 0 { true } else { odd(n - 1) } }\nfunc odd(n: int) -> bool { if n == 0 { false } else { even(n - 1) } }\neven(4)",
            "func apply(f, x) { f(x) }\napply(|n| n + 1, 41)",
            "func outer() { func inner(n: int) -> int { n * 3 }\ninner }\ng := outer()\ng(7)",
            "x := 1\n{ let y = 2\nx + y }",
            "func f() -> int { { return 7 }\n0 }\nf()",
            "func f() -> int { let x = .none\nx? }\nf()",
            "func f() -> result<int, str> { let x = .none\nx? }\nf()",
            "func f() -> result<int, str> { let x = .ok(5)\nx? }\nf()",
            // ---- native loops ----
            "sum := 0\nfor i in 0..5 { sum = sum + i }\nsum",
            "total := 0\nfor n in [10, 20, 30] { total = total + n }\ntotal",
            "found := 0\nfor i in 0..10 { if i == 3 { found = i; break } }\nfound",
            "count := 0\nfor i in 0..5 { if i == 2 { continue }; count = count + 1 }\ncount",
            "for i in 0..3 { i }",
            "for i in 0..0 { i }",
            "for i in [] { i }",
            "sum := 0\nfor i in 0..5 { if i == 2 { continue }\nsum = sum + i }\nsum",
            "sum := 0\nfor i in 0..5 { if i == 2 { break }\nsum = sum + i }\nsum",
            "out := 0\nfor i in 0..3 { for j in 0..3 { out = out + 1 } }\nout",
            "out := 0\nfor i in 0..3 { for j in 0..3 { if j == 1 { break }; out = out + 1 } }\nout",
            "out := 0\nfor i in 0..3 { for j in 0..3 { if j == 1 { continue }; out = out + 1 } }\nout",
            "func f() -> int { for i in 0..3 { if i == 1 { return 42 } }\n0 }\nf()",
            "x := 0\nwhile x < 3 { x = x + 1 }\nx",
            "x := 0\nwhile true { x = x + 1\nif x == 3 { break } }\nx",
            "x := 0\nwhile x < 3 { x = x + 1\nif x == 2 { continue } }\nx",
            "x := 0\nwhile x < 3 { if x == 1 { break }\nx = x + 1 }\nx",
            "func f() -> int { while true { return 7 } }\nf()",
            // ---- native collections ----
            "[1, 2, 3][1]",
            "[[1, 2], [3, 4]][1][0]",
            "[1, 2, 3][-1]",
            "{\"a\": 1, \"b\": 2}[\"b\"]",
            "{1: \"one\", 2: \"two\"}[2]",
            "m := {\"k\": 1}\nm[\"k\"] = 5\nm[\"k\"]",
            "m := {}\nm[\"new\"] = 42\nm[\"new\"]",
            "a := [1, 2, 3]\na[0] = 9\na[0]",
            "a := [1, 2, 3]\na[1] = a[1] * 10\na[1]",
            "[1, 2, 3, 4][1:3]",
            "[1, 2, 3, 4][:2]",
            "[1, 2, 3, 4][2:]",
            "[1, 2, 3, 4][:]",
            "[1, 2, 3, 4][-3:-1]",
            "\"hello\"[1:3]",
            "\"hello\"[:2]",
            "\"hello\"[2:]",
            "\"hello\"[-3:]",
            "\"abc\"[0]",
            "1..5",
            "a := 2\nb := 5\na..b",
            // ---- native structs ----
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x",
            "struct Point { x: int, y: int }\nPoint{ x: 1, y: 2 }.y",
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10\np.x",
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = p.x + 1\np.x",
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x",
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.p.x",
            "struct Bag { items: [int] }\nb := Bag{ items: [1, 2, 3] }\nb.items[1]",
            "struct Bag { items: [int] }\nb := Bag{ items: [1, 2, 3] }\nb.items[1] = 9\nb.items[1]",
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\ndist(Point{ x: 3, y: 4 })",
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nfunc sum(p: Point) -> int { p.x + p.y }\nsum(p)",
            "struct Point { x: int, y: int }\nfunc mk() -> Point { Point{ x: 7, y: 8 } }\nmk().x",
        ] {
            assert_same(src);
        }
    }

    #[test]
    fn vm_deep_recursion_no_rust_stack_overflow() {
        // The VM uses heap-allocated frames, so deep recursion must not
        // overflow the Rust stack (the tree-walker dies around depth 6).
        assert_eq!(
            run_src(
                "func fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(20)"
            )
            .unwrap(),
            Value::Int(6765)
        );
        assert_eq!(
            run_src(
                "func count(n: int) -> int { if n == 0 { 0 } else { count(n - 1) + 1 } }\ncount(100000)"
            )
            .unwrap(),
            Value::Int(100000)
        );
    }

    #[test]
    fn vm_if_condition_must_be_bool() {
        let err = run_src("if 1 { 2 }").unwrap_err();
        assert_eq!(err.message, "`if` condition must be a bool");
    }

    #[test]
    fn vm_undefined_variable_errors() {
        let err = run_src("nope + 1").unwrap_err();
        assert_eq!(err.message, "undefined variable `nope`");
    }

    #[test]
    fn vm_division_by_zero_errors() {
        let err = run_src("1 / 0").unwrap_err();
        assert_eq!(err.message, "division by zero");
    }

    #[test]
    fn vm_return_outside_function_errors() {
        let err = run_src("return 5").unwrap_err();
        assert_eq!(err.message, "`return` outside of a function");
    }

    #[test]
    fn vm_break_outside_loop_errors() {
        let err = run_src("break").unwrap_err();
        assert_eq!(err.message, "`break` outside of a loop");
    }

    #[test]
    fn vm_short_circuit_skips_side_effects() {
        // `false && boom()` must not evaluate `boom` (no error).
        assert_eq!(run_src("false && nope()").unwrap(), Value::Bool(false));
        assert_eq!(run_src("true || nope()").unwrap(), Value::Bool(true));
        // The other side must evaluate.
        assert_eq!(
            run_src("true && nope()").unwrap_err().message,
            "undefined variable `nope`"
        );
        assert_eq!(
            run_src("false || nope()").unwrap_err().message,
            "undefined variable `nope`"
        );
        assert_eq!(run_src("true && 1 < 2").unwrap(), Value::Bool(true));
        assert_eq!(run_src("false || 1 < 2").unwrap(), Value::Bool(true));
    }

    #[test]
    fn vm_method_call_and_cross_module() {
        assert_same(
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\np := Point{ x: 3, y: 4 }\np.dist()",
        );
        // Cross-module: `shapes.Point` receivers resolve `dist` to
        // `shapes.dist`. Dotted struct/function definitions are not
        // parseable, so seed the interpreter state directly (as the loader
        // does) and use the loader-qualified struct-init name.
        let parsed = parse("p := shapes.Point{ x: 3, y: 4 }\np.dist()");
        let mut interp = Interp::new();
        interp
            .structs
            .insert("shapes.Point".into(), vec!["x".into(), "y".into()]);
        let body = parse("p.x + p.y");
        let mut chunk = Compiler::compile_program(&body.program);
        chunk.params = vec![Param {
            name: Ident {
                name: "p".into(),
                span: Span::new(0, 0),
            },
            ty: None,
            span: Span::new(0, 0),
        }];
        let fv = FuncValue {
            params: chunk.params.clone(),
            body: Expr::Block(Block {
                stmts: Vec::new(),
                span: Span::new(0, 0),
            }),
            env: Rc::clone(&interp.env),
            chunk: Some(Rc::new(chunk)),
        };
        interp.funcs.insert("shapes.dist".into(), fv);
        let v = interp.run(&parsed.program).unwrap();
        assert_eq!(v, Value::Int(7));
    }

    #[test]
    fn vm_compiles_expected_opcodes() {
        let parsed = parse("1 + 2 * 3");
        let chunk = Compiler::compile_program(&parsed.program);
        // 1, 2, 3 constants; BinOp Mul; BinOp Add.
        assert!(matches!(
            chunk.code.as_slice(),
            [
                Op::PushConst(_),
                Op::PushConst(_),
                Op::PushConst(_),
                Op::BinOp(BinOp::Mul, _),
                Op::BinOp(BinOp::Add, _),
            ]
        ));
    }

    #[test]
    #[ignore]
    fn bench_loop_vm_vs_tree() {
        let src = "sum := 0\nfor i in 0..100000 { sum = sum + i }\nsum";
        let parsed = parse(src);
        let start = std::time::Instant::now();
        let mut interp = Interp::new();
        let v = interp.run(&parsed.program).unwrap();
        let vm_time = start.elapsed();
        let tree_time = std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn(move || {
                let start = std::time::Instant::now();
                let mut interp = Interp::new();
                let t = interp.run_tree_walker(&parsed.program).unwrap();
                (t.to_string(), start.elapsed())
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(v.to_string(), tree_time.0);
        println!("loop(100k) VM: {vm_time:?}  tree-walker: {:?}", tree_time.1);
    }

    #[test]
    #[ignore]
    fn bench_fib_vm_vs_tree() {
        let src =
            "func fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(20)";
        let parsed = parse(src);
        let start = std::time::Instant::now();
        let mut interp = Interp::new();
        let v = interp.run(&parsed.program).unwrap();
        let vm_time = start.elapsed();
        // The tree-walker dies around depth 6 on the default stack; run it on
        // a dedicated big-stack thread so the comparison is apples-to-apples.
        let tree_time = std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn(move || {
                let start = std::time::Instant::now();
                let mut interp = Interp::new();
                let t = interp.run_tree_walker(&parsed.program).unwrap();
                (t.to_string(), start.elapsed())
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(v.to_string(), tree_time.0);
        println!("fib(20) VM: {vm_time:?}  tree-walker: {:?}", tree_time.1);
    }
}
