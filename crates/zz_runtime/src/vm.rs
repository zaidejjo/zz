//! Phase 6: bytecode compiler and stack-based virtual machine.
//!
//! The compiler lowers the AST into a flat sequence of [`Op`] instructions
//! stored in a [`Chunk`]. The [`Vm`] executes chunks on a shared value stack
//! with an explicit call-frame stack, so function calls do not recurse in
//! Rust.
//!
//! Native bytecode covers the full language: literals, variables, paths,
//! arithmetic, logical operators, calls (incl. method resolution), `if`,
//! blocks, fmt strings, declarations, assignments (incl. index/field
//! write-back), `return`, functions, structs, `for`/`while` loops with
//! `break`/`continue`, arrays, dicts, indexing, slicing, ranges, closures,
//! variants, `match`, `if let`, and `?`.
//!
//! The Phase 1 tree-walker survives only behind `Interp::run_tree_walker`,
//! used by differential tests to cross-check the VM.

use std::cell::RefCell;
use std::rc::Rc;

use zz_frontend::ast::{BinOp, Block, Expr, FmtPart, Param, Pattern, Program, Stmt, UnOp};
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
    /// Push the value of a compile-time-resolved local slot.
    LoadSlot(u16),
    /// Pop a value and write it to a compile-time-resolved local slot.
    StoreSlot(u16),

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
    /// item (bound to `var` in a fresh iteration scope when `in_env`, or left
    /// on the stack as a local slot), or exit when the iterable is exhausted.
    ForNext {
        var: String,
        exit: usize,
        in_env: bool,
    },
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
    /// Pop a value and an array; push the array with the value appended.
    ArrayPush(Span),
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

    // ---- closures & variants ----
    /// Create a closure value from a pre-compiled body chunk, capturing the
    /// current environment.
    MakeClosure {
        params: Vec<Param>,
        chunk: Rc<Chunk>,
    },
    /// Pop an optional argument and push an Option/Result variant.
    MakeVariant {
        name: String,
        has_arg: bool,
        span: Span,
    },

    // ---- pattern matching ----
    /// Pop the scrutinee; try `pat` in a fresh scope (when `has_env`, i.e.
    /// the pattern binds names). On a match, run the body in that scope;
    /// otherwise push the scrutinee back and jump to `next` (the following
    /// arm or the non-exhaustive error).
    MatchArm {
        pat: Pattern,
        next: usize,
        has_env: bool,
    },
    /// Error: no match arm matched.
    MatchError(Span),
    /// Pop the value; try `pat` in a fresh scope (when `has_env`). On a
    /// match, run the `then` block in that scope; otherwise push the value
    /// back and jump to `els`.
    IfLetMatch {
        pat: Pattern,
        els: usize,
        has_env: bool,
    },
    /// Pop a value; unwrap Option/Result or `return` the None/Err variant.
    TryOp(Span),

    // ---- elvis ----
    /// Pop a value; if `.some(v)`, push `v` then `Bool(true)`.
    /// If `.none`, push `Bool(false)`.
    Elvis(Span),
    /// Pop `Bool(success)`, pop right side, pop unwrapped left side.
    /// If success: push unwrapped left. Otherwise: push right side.
    ElvisResult,

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

    /// Call the last path component as a field or method on a receiver that
    /// was loaded from a local slot (`p.dist()` where `p` is a local).
    /// Pops `argc` args, then the receiver.
    CallMethod { name: String, argc: u16, span: Span },

    // ---- fmt ----
    /// Pop `n` values, concatenate their Display forms, push the string.
    Concat(u16),
    /// Pop a format spec string, pop a value, push the formatted string.
    FormatValue(Span),

    // ---- scopes ----
    /// Enter a new child scope (only emitted when the scope declares
    /// captured variables).
    EnterScope,
    /// Leave the current scope, restoring its parent.
    ExitScope,
    /// Pop the top value, discard `n` values below it, and push the value
    /// back: leaves a scope's result while dropping its local slots.
    PopN(u16),
}

/// A compile-time-resolved local variable.
struct Local {
    name: String,
    /// Stack slot index (relative to the frame base). Unused when `in_env`.
    slot: usize,
    /// Scope depth at which the local was declared.
    /// The local lives in the environment (captured by a closure, or a
    /// top-level global) instead of a stack slot.
    in_env: bool,
}

/// How a variable reference resolves.
enum Resolved {
    /// A local stack slot.
    Slot(usize),
    /// The environment chain (global, captured-from-outer, func, or native).
    Env,
}

/// Lowers an AST into a [`Chunk`].
pub struct Compiler {
    chunk: Chunk,
    /// Active locals, innermost last.
    locals: Vec<Local>,
    /// Current lexical scope depth (0 = function/top level).
    scope_depth: usize,
    /// Stack height relative to the frame base, tracked so declarations can
    /// be assigned slot indices.
    stack_height: usize,
    /// Names referenced by nested closures but not defined within them.
    /// Such names must live in the environment so closures can capture them.
    captured: std::collections::HashSet<String>,
    /// True for the top-level program chunk (depth-0 declarations are
    /// globals, stored in the environment).
    is_main: bool,
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
            locals: Vec::new(),
            scope_depth: 0,
            stack_height: 0,
            captured: std::collections::HashSet::new(),
            is_main: false,
        }
    }

    /// Compile a whole program. The top level runs in the interpreter's root
    /// scope (no `EnterScope`), matching the tree-walker.
    pub fn compile_program(program: &Program) -> Chunk {
        let mut c = Compiler::new();
        c.is_main = true;
        for (i, stmt) in program.stmts.iter().enumerate() {
            let v = c.compile_stmt(stmt);
            if i < program.stmts.len() - 1 && matches!(v, StmtValue::Discard) {
                c.emit(Op::Pop);
            }
        }
        if program.stmts.is_empty() {
            c.emit_const(Value::Unit);
        }
        c.chunk
    }

    fn emit(&mut self, op: Op) {
        let effect = Self::stack_effect(&op);
        // Use saturating arithmetic to prevent wrap-around on underflow.
        // This can happen when stack_effect is negative and stack_height is 0,
        // which indicates a compiler bug in stack tracking but shouldn't panic.
        self.stack_height = self.stack_height.saturating_add_signed(effect as isize);
        self.chunk.code.push(op);
    }

    /// Net stack effect of an instruction (values pushed minus popped).
    fn stack_effect(op: &Op) -> i64 {
        match op {
            Op::PushConst(_) => 1,
            Op::Pop => -1,
            Op::Truthy => 0,
            Op::LoadVar(..) | Op::LoadPath(..) | Op::LoadSlot(_) => 1,
            Op::DefineVar(_) => 0,
            Op::StoreVar(..) | Op::StorePath(..) | Op::StoreSlot(_) => -1,
            Op::MakeFunc { .. } | Op::RegisterStruct { .. } | Op::MakeClosure { .. } => 1,
            Op::BinOp(..) | Op::UnOp(..) => -1,
            Op::Jump(_) => 0,
            Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::JumpIfFalseBool(..) => -1,
            Op::Return => -1,
            Op::ForSetup { .. } => 2,
            Op::ForNext { .. } => 0,
            Op::WhileSetup { .. } => 0,
            Op::WhileCond { .. } => -1,
            Op::Break | Op::Continue => 0,
            Op::SetLoopResult => -1,
            Op::MakeArray(n) => 1 - *n as i64,
            Op::ArrayPush(_) => -1,
            Op::MakeDict(n) => 1 - 2 * *n as i64,
            Op::IndexOp(_) => -1,
            Op::StoreIndexOp(_) => -2,
            Op::SliceOp(_) => -2,
            Op::MakeRange(_) => -1,
            Op::MakeStruct { field_names, .. } => 1 - field_names.len() as i64,
            Op::GetField(..) => 0,
            Op::SetField(..) => -1,
            Op::MakeVariant { has_arg, .. } => {
                if *has_arg {
                    0
                } else {
                    1
                }
            }
            Op::MatchArm { .. } => -1,
            Op::MatchError(_) => 0,
            Op::IfLetMatch { .. } => -1,
            Op::TryOp(_) => 0,
            Op::Elvis(_) => 1,
            Op::ElvisResult => -2,
            // Call pops argc args + 1 callee (pushed by compiler), pushes 1 result: -argc
            // CallPath pops argc args, resolves callee by name (no stack pop), pushes 1: 1-argc
            // CallMethod pops argc args + 1 receiver (LoadSlot), pushes 1 result: -argc
            Op::Call { argc, .. } | Op::CallMethod { argc, .. } => -(*argc as i64),
            Op::CallPath { argc, .. } => 1 - (*argc as i64),
            Op::Concat(n) => 1 - *n as i64,
            Op::FormatValue(_) => -1,
            Op::EnterScope | Op::ExitScope => 0,
            Op::PopN(n) => -(*n as i64),
        }
    }

    /// Declare a local variable. The initializer value is already on top of
    /// the stack. Captured names and top-level globals go to the environment;
    /// everything else becomes a stack slot. Returns `true` if the value
    /// stays on the stack as the slot's storage (caller must not pop it).
    fn declare_local(&mut self, name: &str) -> bool {
        if self.captured.contains(name) || (self.is_main && self.scope_depth == 0) {
            self.emit(Op::DefineVar(name.to_string()));
            self.locals.push(Local {
                name: name.to_string(),
                slot: 0,
                in_env: true,
            });
            false
        } else {
            // The initializer value is the top of the stack.
            let slot = self.stack_height - 1;
            self.locals.push(Local {
                name: name.to_string(),
                slot,
                in_env: false,
            });
            true
        }
    }

    /// Resolve a variable reference to a slot or the environment.
    fn resolve(&self, name: &str) -> Resolved {
        for local in self.locals.iter().rev() {
            if local.name == name {
                return if local.in_env {
                    Resolved::Env
                } else {
                    Resolved::Slot(local.slot)
                };
            }
        }
        Resolved::Env
    }

    /// Compile a dotted path load. If the root resolves to a local slot,
    /// load the slot and walk the remaining fields; otherwise use the
    /// environment-based `LoadPath`.
    fn compile_path_load(&mut self, parts: &[String], span: Span) {
        if let Resolved::Slot(slot) = self.resolve(&parts[0]) {
            self.emit(Op::LoadSlot(slot as u16));
            for part in &parts[1..] {
                self.emit(Op::GetField(part.clone(), span));
            }
        } else {
            self.emit(Op::LoadPath(parts.to_vec(), span));
        }
    }

    /// Compile a dotted path store (write-back). The value is on top of the
    /// stack. For a slot root: load the object chain, set the innermost
    /// field, then walk back up writing each level into its parent so the
    /// root keeps its shape — mirroring the tree-walker's `assign_path`.
    fn compile_path_store(&mut self, parts: &[String], span: Span) {
        if let Resolved::Slot(slot) = self.resolve(&parts[0]) {
            self.emit(Op::LoadSlot(slot as u16));
            for part in &parts[1..parts.len() - 1] {
                self.emit(Op::GetField(part.clone(), span));
            }
            self.emit(Op::SetField(parts.last().unwrap().clone(), span));
            // Write each intermediate level back into its parent: reload the
            // root, walk up to (but not including) the level, set the field.
            for i in (1..parts.len() - 1).rev() {
                self.emit(Op::LoadSlot(slot as u16));
                for part in &parts[1..i] {
                    self.emit(Op::GetField(part.clone(), span));
                }
                self.emit(Op::SetField(parts[i].clone(), span));
            }
            self.emit(Op::StoreSlot(slot as u16));
        } else {
            self.emit(Op::StorePath(parts.to_vec(), span));
        }
    }

    /// Whether a block's direct statements declare any captured variable
    /// (which forces the scope to enter an environment).
    fn scope_declares_captured(&self, block: &Block) -> bool {
        block.stmts.iter().any(|s| match s {
            Stmt::Decl { name, .. } => self.captured.contains(&name.name),
            _ => false,
        })
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
            Op::ForNext { var, in_env, .. } => Op::ForNext {
                var,
                exit: target,
                in_env,
            },
            Op::WhileCond { span, .. } => Op::WhileCond { exit: target, span },
            other => panic!("patch_jump on non-jump op: {other:?}"),
        };
    }

    fn emit_for_next(&mut self, var: &str, in_env: bool) -> usize {
        let pos = self.chunk.code.len();
        self.emit(Op::ForNext {
            var: var.to_string(),
            exit: 0,
            in_env,
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
            Expr::Ident { name, span } => match self.resolve(name) {
                Resolved::Slot(slot) => self.emit(Op::StoreSlot(slot as u16)),
                Resolved::Env => self.emit(Op::StoreVar(name.clone(), *span)),
            },
            Expr::Path { parts, span } => self.compile_path_store(parts, *span),
            Expr::Field { obj, name, span } => {
                self.compile_expr(obj);
                self.emit(Op::SetField(name.clone(), *span));
                self.compile_write_back(obj);
            }
            _ => self.emit(Op::Pop),
        }
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> StmtValue {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                self.compile_expr(value);
                if self.declare_local(&name.name) {
                    StmtValue::Keep
                } else {
                    StmtValue::Discard
                }
            }
            Stmt::Import { .. } => {
                // Imports are resolved by the loader/session; no runtime effect.
                StmtValue::None
            }
            Stmt::Func {
                name, params, body, ..
            } => {
                let chunk = self.compile_func_body(body, params);
                self.emit(Op::MakeFunc {
                    name: name.join("."),
                    params: params.clone(),
                    chunk,
                });
                StmtValue::Discard
            }
            Stmt::Return { value, .. } => {
                match value {
                    Some(e) => self.compile_expr(e),
                    None => self.emit_const(Value::Unit),
                }
                self.emit(Op::Return);
                StmtValue::None
            }
            Stmt::Struct { name, fields, .. } => {
                self.emit(Op::RegisterStruct {
                    name: name.join("."),
                    fields: fields.iter().map(|(n, _)| n.name.clone()).collect(),
                });
                StmtValue::Discard
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
                let pre = self.stack_height;
                self.emit_const(Value::Unit);
                self.compile_expr(iter);
                let setup_pos = self.chunk.code.len();
                self.emit(Op::ForSetup {
                    exit: 0,
                    header: 0,
                    span: *span,
                });
                let header = self.chunk.code.len();
                let in_env = self.captured.contains(&var.name);
                let j = self.emit_for_next(&var.name, in_env);
                // The loop variable: a slot (item left on the stack by
                // ForNext) or an environment binding (captured).
                self.locals.push(Local {
                    name: var.name.clone(),
                    slot: self.stack_height - 1,
                    in_env,
                });
                // The loop body runs in its own scope so its locals are
                // popped every iteration.
                let body_needs_env = self.scope_declares_captured(body);
                if body_needs_env {
                    self.emit(Op::EnterScope);
                }
                self.scope_depth += 1;
                self.compile_block_body(body);
                self.scope_depth -= 1;
                if body_needs_env {
                    self.emit(Op::ExitScope);
                }
                self.emit(Op::SetLoopResult);
                self.emit(Op::Jump(header));
                self.patch_jump(j);
                let exit = self.chunk.code.len();
                self.chunk.code[setup_pos] = Op::ForSetup {
                    exit,
                    header,
                    span: *span,
                };
                self.locals.pop();
                // The loop leaves exactly one value: its result.
                self.stack_height = pre + 1;
                StmtValue::Discard
            }
            Stmt::Break { .. } => {
                self.emit(Op::Break);
                StmtValue::None
            }
            Stmt::Continue { .. } => {
                self.emit(Op::Continue);
                StmtValue::None
            }
            Stmt::Assign { target, value, .. } => match target {
                Expr::Ident { name, span } => {
                    self.compile_expr(value);
                    match self.resolve(name) {
                        Resolved::Slot(slot) => self.emit(Op::StoreSlot(slot as u16)),
                        Resolved::Env => self.emit(Op::StoreVar(name.clone(), *span)),
                    }
                    self.emit_const(Value::Unit);
                    StmtValue::Discard
                }
                Expr::Path { parts, span } => {
                    self.compile_expr(value);
                    self.compile_path_store(parts, *span);
                    self.emit_const(Value::Unit);
                    StmtValue::Discard
                }
                Expr::Index { obj, index, span } => {
                    self.compile_expr(value);
                    self.compile_expr(index);
                    self.compile_expr(obj);
                    self.emit(Op::StoreIndexOp(*span));
                    self.compile_write_back(obj);
                    self.emit_const(Value::Unit);
                    StmtValue::Discard
                }
                Expr::Field { obj, name, span } => {
                    self.compile_expr(value);
                    self.compile_expr(obj);
                    self.emit(Op::SetField(name.clone(), *span));
                    // The tree-walker writes a mutated object back only when
                    // the base is a plain variable.
                    self.compile_write_back(obj);
                    self.emit_const(Value::Unit);
                    StmtValue::Discard
                }
                _ => unreachable!("unhandled assignment target"),
            },
            Stmt::Expr(e) => {
                self.compile_expr(e);
                StmtValue::Discard
            }
        }
    }

    /// Compile a block: child scope, statements (non-final values popped),
    /// final value left on the stack, scope exit.
    fn compile_block(&mut self, block: &Block) {
        let needs_env = self.scope_declares_captured(block);
        if needs_env {
            self.emit(Op::EnterScope);
        }
        self.scope_depth += 1;
        self.compile_block_body(block);
        self.scope_depth -= 1;
        if needs_env {
            self.emit(Op::ExitScope);
        }
    }

    /// Compile a block's statements within the current scope, popping the
    /// scope's local slots at the end (keeping the block's value). Used for
    /// loop bodies, function bodies, and closure bodies.
    fn compile_block_body(&mut self, block: &Block) {
        let scope_base = self.locals.len();
        let mut last = StmtValue::None;
        for (i, stmt) in block.stmts.iter().enumerate() {
            last = self.compile_stmt(stmt);
            // Discard the value of non-final statements. `Keep` (a slot
            // declaration) stays on the stack as the slot's storage.
            if i < block.stmts.len() - 1 && matches!(last, StmtValue::Discard) {
                self.emit(Op::Pop);
            }
        }
        if block.stmts.is_empty() || matches!(last, StmtValue::None) {
            self.emit_const(Value::Unit);
        }
        // Pop only slot locals; env locals' values were already discarded by
        // their statement pops (or remain as the block value).
        let n = self.locals[scope_base..]
            .iter()
            .filter(|l| !l.in_env)
            .count();
        if n > 0 {
            self.emit(Op::PopN(n as u16));
        }
        self.locals.truncate(scope_base);
    }

    /// Compile a function body into its own chunk. The parameter list is
    /// stored on the chunk so `call_func` can arity-check it. Parameters
    /// become stack slots; captured parameters are copied into the
    /// environment at entry.
    fn compile_func_body(&mut self, block: &Block, params: &[Param]) -> Rc<Chunk> {
        let mut sub = Compiler::new();
        sub.chunk.params = params.to_vec();
        sub.captured = scan_block_captured(block, params);
        let needs_env = params.iter().any(|p| sub.captured.contains(&p.name.name))
            || sub.scope_declares_captured(block);
        if needs_env {
            sub.emit(Op::EnterScope);
        }
        for (i, p) in params.iter().enumerate() {
            if sub.captured.contains(&p.name.name) {
                sub.emit(Op::LoadSlot(i as u16));
                sub.emit(Op::DefineVar(p.name.name.clone()));
                sub.locals.push(Local {
                    name: p.name.name.clone(),
                    slot: i,
                    in_env: true,
                });
            } else {
                sub.locals.push(Local {
                    name: p.name.name.clone(),
                    slot: i,
                    in_env: false,
                });
            }
        }
        sub.stack_height = params.len();
        sub.compile_block_body(block);
        if needs_env {
            sub.emit(Op::ExitScope);
        }
        Rc::new(sub.chunk)
    }

    /// Compile a closure body (an expression) into its own chunk.
    fn compile_closure_body(&mut self, body: &Expr, params: &[Param]) -> Rc<Chunk> {
        let mut sub = Compiler::new();
        sub.chunk.params = params.to_vec();
        sub.captured = scan_closure_captured(body, params);
        let needs_env = params.iter().any(|p| sub.captured.contains(&p.name.name));
        if needs_env {
            sub.emit(Op::EnterScope);
        }
        for (i, p) in params.iter().enumerate() {
            if sub.captured.contains(&p.name.name) {
                sub.emit(Op::LoadSlot(i as u16));
                sub.emit(Op::DefineVar(p.name.name.clone()));
                sub.locals.push(Local {
                    name: p.name.name.clone(),
                    slot: i,
                    in_env: true,
                });
            } else {
                sub.locals.push(Local {
                    name: p.name.name.clone(),
                    slot: i,
                    in_env: false,
                });
            }
        }
        sub.stack_height = params.len();
        sub.compile_expr(body);
        if needs_env {
            sub.emit(Op::ExitScope);
        }
        Rc::new(sub.chunk)
    }

    fn compile_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { value, .. } => self.emit_const(Value::Int(*value)),
            Expr::Float { value, .. } => self.emit_const(Value::Float(*value)),
            Expr::Str { value, .. } => self.emit_const(Value::Str(value.clone())),
            Expr::Bool { value, .. } => self.emit_const(Value::Bool(*value)),
            Expr::Ident { name, span } => match self.resolve(name) {
                Resolved::Slot(slot) => self.emit(Op::LoadSlot(slot as u16)),
                Resolved::Env => self.emit(Op::LoadVar(name.clone(), *span)),
            },
            Expr::Path { parts, span } => self.compile_path_load(parts, *span),
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
                BinOp::Elvis => {
                    self.compile_expr(left);
                    self.emit(Op::Elvis(*span));
                    self.compile_expr(right);
                    self.emit(Op::ElvisResult);
                }
                _ => {
                    self.compile_expr(left);
                    self.compile_expr(right);
                    self.emit(Op::BinOp(*op, *span));
                }
            },
            Expr::Call { callee, args, span } => match callee.as_ref() {
                Expr::Path { parts, span: pspan } => {
                    // Special case: `input()` with no args -> synthesize empty prompt
                    let is_input = parts.len() == 1 && parts[0] == "input" && args.is_empty();
                    // Special case: `range` with 1 or 2 args -> synthesize defaults (start=0, step=1)
                    let is_range = parts.len() == 1 && parts[0] == "range" && args.len() < 3;
                    let argc = if is_input {
                        1
                    } else if is_range {
                        3
                    } else {
                        args.len()
                    };
                    if let Resolved::Slot(slot) = self.resolve(&parts[0]) {
                        // Slot-based method dispatch: push receiver first, then
                        // args. CallMethod pops argc args (on top) then pops recv.
                        self.emit(Op::LoadSlot(slot as u16));
                        for part in &parts[1..parts.len() - 1] {
                            self.emit(Op::GetField(part.clone(), *pspan));
                        }
                        if is_range {
                            if args.len() == 1 {
                                self.emit_const(Value::Int(0));
                                self.compile_expr(&args[0]);
                                self.emit_const(Value::Int(1));
                            } else if args.len() == 2 {
                                self.compile_expr(&args[0]);
                                self.compile_expr(&args[1]);
                                self.emit_const(Value::Int(1));
                            } else {
                                for a in args {
                                    self.compile_expr(a);
                                }
                            }
                        } else {
                            for a in args {
                                self.compile_expr(a);
                            }
                        }
                        if is_input {
                            self.emit_const(Value::Str(String::new()));
                        }
                        self.emit(Op::CallMethod {
                            name: parts.last().unwrap().clone(),
                            argc: argc as u16,
                            span: *span,
                        });
                    } else {
                        // Path-based dispatch: args on stack, resolved at runtime.
                        if is_range {
                            if args.len() == 1 {
                                self.emit_const(Value::Int(0));
                                self.compile_expr(&args[0]);
                                self.emit_const(Value::Int(1));
                            } else if args.len() == 2 {
                                self.compile_expr(&args[0]);
                                self.compile_expr(&args[1]);
                                self.emit_const(Value::Int(1));
                            } else {
                                for a in args {
                                    self.compile_expr(a);
                                }
                            }
                        } else {
                            for a in args {
                                self.compile_expr(a);
                            }
                        }
                        if is_input {
                            self.emit_const(Value::Str(String::new()));
                        }
                        self.emit(Op::CallPath {
                            parts: parts.clone(),
                            argc: argc as u16,
                            span: *span,
                            pspan: *pspan,
                        });
                    }
                }
                // Method call on expression: `expr.method(args)` — push receiver
                // first, then args. CallMethod pops argc args (on top) then pops recv.
                Expr::Field { obj, name, span: _ } => {
                    self.compile_expr(obj);
                    for a in args {
                        self.compile_expr(a);
                    }
                    self.emit(Op::CallMethod {
                        name: name.clone(),
                        argc: args.len() as u16,
                        span: *span,
                    });
                }
                _ => {
                    // Special case: `input()` with no args -> synthesize empty prompt
                    let is_input = matches!(callee.as_ref(), Expr::Ident { name, .. } if name == "input")
                        && args.is_empty();
                    // Special case: `range` with 1 or 2 args -> synthesize defaults
                    let is_range = matches!(callee.as_ref(), Expr::Ident { name, .. } if name == "range")
                        && args.len() < 3;
                    let argc = if is_input {
                        1
                    } else if is_range {
                        3
                    } else {
                        args.len()
                    };
                    self.compile_expr(callee);
                    if is_range {
                        if args.len() == 1 {
                            self.emit_const(Value::Int(0));
                            self.compile_expr(&args[0]);
                            self.emit_const(Value::Int(1));
                        } else if args.len() == 2 {
                            self.compile_expr(&args[0]);
                            self.compile_expr(&args[1]);
                            self.emit_const(Value::Int(1));
                        } else {
                            for a in args {
                                self.compile_expr(a);
                            }
                        }
                    } else {
                        for a in args {
                            self.compile_expr(a);
                        }
                    }
                    if is_input {
                        self.emit_const(Value::Str(String::new()));
                    }
                    self.emit(Op::Call {
                        argc: argc as u16,
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
                let pre = self.stack_height;
                self.compile_expr(cond);
                let j = self.emit_jump(JumpKind::IfFalseBool(*span));
                self.compile_block(then);
                let j2 = self.emit_jump(JumpKind::Always);
                self.patch_jump(j);
                // Reset tracker: the else branch starts from the same stack
                // state as the then branch (after the condition was consumed),
                // not from the height left by the then branch.
                self.stack_height = pre;
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
                        FmtPart::Expr(e, Some(spec)) => {
                            self.compile_expr(e);
                            self.emit_const(Value::Str(spec.clone()));
                            self.emit(Op::FormatValue(e.span()));
                            n += 1;
                        }
                        FmtPart::Expr(e, None) => {
                            self.compile_expr(e);
                            n += 1;
                        }
                    }
                }
                self.emit(Op::Concat(n));
            }
            Expr::While { cond, body, span } => {
                let pre = self.stack_height;
                let setup_pos = self.chunk.code.len();
                self.emit(Op::WhileSetup { exit: 0, header: 0 });
                self.emit_const(Value::Unit);
                let header = self.chunk.code.len();
                self.compile_expr(cond);
                let j = self.emit_while_cond(*span);
                let body_needs_env = self.scope_declares_captured(body);
                if body_needs_env {
                    self.emit(Op::EnterScope);
                }
                self.scope_depth += 1;
                self.compile_block_body(body);
                self.scope_depth -= 1;
                if body_needs_env {
                    self.emit(Op::ExitScope);
                }
                self.emit(Op::SetLoopResult);
                self.emit(Op::Jump(header));
                self.patch_jump(j);
                let exit = self.chunk.code.len();
                self.chunk.code[setup_pos] = Op::WhileSetup { exit, header };
                // The loop leaves exactly one value: its result.
                self.stack_height = pre + 1;
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.compile_expr(e);
                }
                self.emit(Op::MakeArray(elems.len() as u16));
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                span,
            } => {
                // Reserve a slot for the result array.
                let result_slot = self.stack_height;
                self.emit_const(Value::Unit); // placeholder
                                              // Create empty result array and store it in the reserved slot.
                self.emit(Op::MakeArray(0));
                self.emit(Op::StoreSlot(result_slot as u16));
                // Compile the iterator (now at stack bottom).
                self.compile_expr(iter);
                // Set up the for loop. ForSetup pops the iterable, pushes
                // iterable, idx, placeholder. Result array stays in its slot.
                let setup_pos = self.chunk.code.len();
                self.emit(Op::ForSetup {
                    exit: 0,
                    header: 0,
                    span: *span,
                });
                let header = self.chunk.code.len();
                let in_env = self.captured.contains(&var.name);
                let j = self.emit_for_next(&var.name, in_env);
                // The loop variable: a slot (item left on the stack by ForNext)
                // or an environment binding (captured).
                // IMPORTANT: Add the local BEFORE compiling filter/body so the
                // variable resolves correctly in those expressions.
                self.locals.push(Local {
                    name: var.name.clone(),
                    slot: self.stack_height - 1,
                    in_env,
                });
                // Loop body: load result array from slot, compile body, append, store back.
                if let Some(f) = filter {
                    self.compile_expr(f);
                    // If filter is false, jump to header (next iteration).
                    self.emit(Op::JumpIfFalse(header));
                    // Load result array, compile body, append, store back.
                    self.emit(Op::LoadSlot(result_slot as u16));
                    self.compile_expr(body);
                    self.emit(Op::ArrayPush(*span));
                    self.emit(Op::StoreSlot(result_slot as u16));
                    // Jump back to loop header.
                    self.emit(Op::Jump(header));
                } else {
                    // No filter: load result array, compile body, append, store back.
                    self.emit(Op::LoadSlot(result_slot as u16));
                    self.compile_expr(body);
                    self.emit(Op::ArrayPush(*span));
                    self.emit(Op::StoreSlot(result_slot as u16));
                    self.emit(Op::Jump(header));
                }
                self.patch_jump(j);
                let exit = self.chunk.code.len();
                self.chunk.code[setup_pos] = Op::ForSetup {
                    exit,
                    header,
                    span: *span,
                };
                self.locals.pop();
                // After ForNext truncation, the result array is already the
                // only value on the stack at position `result_slot`. No need
                // to LoadSlot — doing so would push a duplicate that leaks
                // into subsequent comprehensions, shifting slot offsets.
                self.stack_height = result_slot + 1;
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
            Expr::Closure { params, body, .. } => {
                let chunk = self.compile_closure_body(body, params);
                self.emit(Op::MakeClosure {
                    params: params.clone(),
                    chunk,
                });
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                self.compile_expr(scrutinee);
                let mut arm_positions = Vec::with_capacity(arms.len());
                let mut body_jumps = Vec::with_capacity(arms.len());
                for arm in arms {
                    let pos = self.chunk.code.len();
                    let has_env = pattern_binds(&arm.pat);
                    self.emit(Op::MatchArm {
                        pat: arm.pat.clone(),
                        next: 0,
                        has_env,
                    });
                    self.compile_expr(&arm.body);
                    if has_env {
                        self.emit(Op::ExitScope);
                    }
                    let j = self.emit_jump(JumpKind::Always);
                    arm_positions.push(pos);
                    body_jumps.push(j);
                }
                let error_pos = self.chunk.code.len();
                self.emit(Op::MatchError(*span));
                for (i, pos) in arm_positions.iter().enumerate() {
                    let next = if i + 1 < arms.len() {
                        arm_positions[i + 1]
                    } else {
                        error_pos
                    };
                    self.chunk.code[*pos] = Op::MatchArm {
                        pat: arms[i].pat.clone(),
                        next,
                        has_env: pattern_binds(&arms[i].pat),
                    };
                }
                for j in body_jumps {
                    self.patch_jump(j);
                }
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                span: _,
            } => {
                let pre = self.stack_height;
                self.compile_expr(value);
                let has_env = pattern_binds(pat);
                let pos = self.chunk.code.len();
                self.emit(Op::IfLetMatch {
                    pat: pat.clone(),
                    els: 0,
                    has_env,
                });
                self.compile_block(then);
                if has_env {
                    self.emit(Op::ExitScope);
                }
                let j = self.emit_jump(JumpKind::Always);
                let els_pos = self.chunk.code.len();
                // Reset tracker: else branch starts from same stack state
                // as then branch (after the scrutinee was consumed).
                self.stack_height = pre;
                self.emit(Op::Pop);
                match els {
                    Some(e) => self.compile_expr(e),
                    None => self.emit_const(Value::Unit),
                }
                self.chunk.code[pos] = Op::IfLetMatch {
                    pat: pat.clone(),
                    els: els_pos,
                    has_env,
                };
                self.patch_jump(j);
            }
            Expr::Try { expr, span } => {
                self.compile_expr(expr);
                self.emit(Op::TryOp(*span));
            }
            Expr::Variant { name, arg, span } => {
                if let Some(a) = arg {
                    self.compile_expr(a);
                }
                self.emit(Op::MakeVariant {
                    name: name.clone(),
                    has_arg: arg.is_some(),
                    span: *span,
                });
            }
        }
    }
}

/// What a compiled statement leaves on the stack.
#[derive(Clone, Copy, PartialEq)]
enum StmtValue {
    /// A slot declaration: the value stays as the slot's storage.
    Keep,
    /// A value that should be discarded by the caller.
    Discard,
    /// Nothing on the stack.
    None,
}

/// Whether a pattern binds any names (forcing a scope environment).
fn pattern_binds(pat: &Pattern) -> bool {
    match pat {
        Pattern::Binding { .. } => true,
        Pattern::Variant { arg: Some(p), .. } => pattern_binds(p),
        _ => false,
    }
}

/// Collect the names referenced by nested closures but not defined within
/// them. These must live in the environment so closures can capture them.
fn scan_block_captured(block: &Block, params: &[Param]) -> std::collections::HashSet<String> {
    let mut defined: std::collections::HashSet<String> =
        params.iter().map(|p| p.name.name.clone()).collect();
    let mut free = std::collections::HashSet::new();
    for stmt in &block.stmts {
        scan_stmt_captured(stmt, &mut defined, &mut free);
    }
    free
}

/// Like [`scan_block_captured`] but for a closure body (an expression).
fn scan_closure_captured(body: &Expr, params: &[Param]) -> std::collections::HashSet<String> {
    let mut defined: std::collections::HashSet<String> =
        params.iter().map(|p| p.name.name.clone()).collect();
    let mut free = std::collections::HashSet::new();
    scan_expr_captured(body, &mut defined, &mut free);
    free
}

fn scan_expr_captured(
    expr: &Expr,
    defined: &mut std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
) {
    match expr {
        Expr::Ident { name, .. } => {
            if !defined.contains(name) {
                free.insert(name.clone());
            }
        }
        // A closure's free variables are relative to the closure itself:
        // its params and its own body locals. The enclosing function's
        // locals are free from the closure's perspective.
        Expr::Closure { params, body, .. } => {
            let mut inner: std::collections::HashSet<String> =
                params.iter().map(|p| p.name.name.clone()).collect();
            scan_expr_captured(body, &mut inner, free);
        }
        Expr::Block(block) => {
            let mut inner = defined.clone();
            for stmt in &block.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
        }
        Expr::Paren { expr, .. } => scan_expr_captured(expr, defined, free),
        Expr::Unary { expr, .. } => scan_expr_captured(expr, defined, free),
        Expr::Binary { left, right, .. } => {
            scan_expr_captured(left, defined, free);
            scan_expr_captured(right, defined, free);
        }
        Expr::Call { callee, args, .. } => {
            scan_expr_captured(callee, defined, free);
            for a in args {
                scan_expr_captured(a, defined, free);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            scan_expr_captured(cond, defined, free);
            let mut inner = defined.clone();
            for stmt in &then.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
            if let Some(e) = els {
                scan_expr_captured(e, defined, free);
            }
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let FmtPart::Expr(e, _) = part {
                    scan_expr_captured(e, defined, free);
                }
            }
        }
        Expr::While { cond, body, .. } => {
            scan_expr_captured(cond, defined, free);
            let mut inner = defined.clone();
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            scan_expr_captured(scrutinee, defined, free);
            for arm in arms {
                let mut inner = defined.clone();
                collect_pattern_bindings(&arm.pat, &mut inner);
                scan_expr_captured(&arm.body, &mut inner, free);
            }
        }
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            ..
        } => {
            scan_expr_captured(value, defined, free);
            let mut inner = defined.clone();
            collect_pattern_bindings(pat, &mut inner);
            for stmt in &then.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
            if let Some(e) = els {
                scan_expr_captured(e, defined, free);
            }
        }
        Expr::Try { expr, .. } => scan_expr_captured(expr, defined, free),
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                scan_expr_captured(a, defined, free);
            }
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                scan_expr_captured(e, defined, free);
            }
        }
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            ..
        } => {
            scan_expr_captured(iter, defined, free);
            let mut inner = defined.clone();
            inner.insert(var.name.clone());
            if let Some(f) = filter {
                scan_expr_captured(f, &mut inner, free);
            }
            scan_expr_captured(body, &mut inner, free);
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                scan_expr_captured(k, defined, free);
                scan_expr_captured(v, defined, free);
            }
        }
        Expr::Field { obj, .. } => scan_expr_captured(obj, defined, free),
        Expr::Range { start, end, .. } => {
            scan_expr_captured(start, defined, free);
            scan_expr_captured(end, defined, free);
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                scan_expr_captured(v, defined, free);
            }
        }
        Expr::Index { obj, index, .. } => {
            scan_expr_captured(obj, defined, free);
            scan_expr_captured(index, defined, free);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            scan_expr_captured(obj, defined, free);
            if let Some(e) = start {
                scan_expr_captured(e, defined, free);
            }
            if let Some(e) = end {
                scan_expr_captured(e, defined, free);
            }
        }
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Path { .. } => {}
    }
}

fn scan_stmt_captured(
    stmt: &Stmt,
    defined: &mut std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
) {
    match stmt {
        Stmt::Decl { name, value, .. } => {
            scan_expr_captured(value, defined, free);
            defined.insert(name.name.clone());
        }
        Stmt::Import { .. } => {}
        Stmt::Func {
            name, params, body, ..
        } => {
            let mut inner: std::collections::HashSet<String> =
                params.iter().map(|p| p.name.name.clone()).collect();
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
            defined.insert(name.join("."));
        }
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                scan_expr_captured(e, defined, free);
            }
        }
        Stmt::Struct { .. } => {}
        Stmt::For {
            var, iter, body, ..
        } => {
            scan_expr_captured(iter, defined, free);
            let mut inner = defined.clone();
            inner.insert(var.name.clone());
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Assign { target, value, .. } => {
            scan_expr_captured(value, defined, free);
            scan_expr_captured(target, defined, free);
        }
        Stmt::Expr(e) => scan_expr_captured(e, defined, free),
    }
}

/// Add a pattern's binding names to `defined`.
fn collect_pattern_bindings(pat: &Pattern, defined: &mut std::collections::HashSet<String>) {
    match pat {
        Pattern::Binding { name } => {
            defined.insert(name.name.clone());
        }
        Pattern::Variant { arg: Some(p), .. } => collect_pattern_bindings(p, defined),
        _ => {}
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

    /// Push a value onto the VM stack. Used by `Interp::call_func` to set up
    /// compiled closure parameters before calling `run_chunk_with_base`.
    pub(crate) fn push(&mut self, v: Value) {
        self.stack.push(v);
    }

    /// Run a chunk to completion. Returns the chunk's value, or a control
    /// flow signal (`Return`/`Break`/`Continue`) that escaped the program
    /// frame.
    pub(crate) fn run_chunk(
        &mut self,
        chunk: &Rc<Chunk>,
        interp: &mut Interp,
    ) -> Result<Flow, EvalError> {
        self.run_chunk_with_base(chunk, interp, self.stack.len())
    }

    /// Like `run_chunk`, but allows the caller to specify `stack_base`
    /// explicitly. Used by `Interp::call_func` to run compiled closures
    /// where args are already on the stack at index 0..n.
    pub(crate) fn run_chunk_with_base(
        &mut self,
        chunk: &Rc<Chunk>,
        interp: &mut Interp,
        stack_base: usize,
    ) -> Result<Flow, EvalError> {
        self.frames.push(Frame {
            chunk: Rc::clone(chunk),
            ip: 0,
            prev_env: Rc::clone(&interp.env),
            stack_base,
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
                Op::LoadSlot(slot) => {
                    let base = self.frames.last().unwrap().stack_base;
                    let v = self.stack[base + *slot as usize].clone();
                    self.stack.push(v);
                }
                Op::StoreSlot(slot) => {
                    let v = self.stack.pop().unwrap();
                    let base = self.frames.last().unwrap().stack_base;
                    self.stack[base + *slot as usize] = v;
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
                        Value::Range(start, _, _) => (it, Value::Int(start)),
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
                        slots: 3,
                    });
                    self.stack.push(iterable);
                    self.stack.push(idx);
                    // Placeholder for the loop variable's slot; ForNext pops
                    // it (or the previous item) before pushing the next one.
                    self.stack.push(Value::Unit);
                }
                Op::ForNext { var, exit, in_env } => {
                    // Pop the previous item (or the placeholder), then the
                    // index counter.
                    self.stack.pop().unwrap();
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
                        (Value::Range(_, end, step), Value::Int(i)) => {
                            let i = *i;
                            let step = *step;
                            if step > 0 {
                                if i >= *end {
                                    (true, Value::Unit)
                                } else {
                                    (false, Value::Int(i))
                                }
                            } else {
                                if i <= *end {
                                    (true, Value::Unit)
                                } else {
                                    (false, Value::Int(i))
                                }
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
                        let next = match (&self.stack[iterable_idx], idx) {
                            (Value::Range(_, _, step), Value::Int(i)) => Value::Int(i + step),
                            (Value::Array(_), Value::Int(i)) => Value::Int(i + 1),
                            _ => unreachable!(),
                        };
                        self.stack.push(next);
                        self.stack.push(item.clone());
                        if *in_env {
                            let li = self.loops.last().unwrap();
                            let loop_env = Rc::clone(&li.env);
                            interp.env = loop_env;
                            let scope = Env::with_parent(&interp.env);
                            scope.borrow_mut().define(var, item);
                            interp.env = scope;
                        }
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
                Op::ArrayPush(span) => {
                    let value = self.stack.pop().unwrap();
                    let mut arr = match self.stack.pop().unwrap() {
                        Value::Array(a) => a,
                        other => {
                            return Err(EvalError::new(
                                format!("ArrayPush: expected array, found `{other}`"),
                                *span,
                            ));
                        }
                    };
                    arr.push(value);
                    self.stack.push(Value::Array(arr));
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
                        (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Range(a, b, 1)),
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
                    let mut ov = self.stack.pop().unwrap();
                    let value = self.stack.pop().unwrap();
                    Interp::set_object_field(&mut ov, name, value, *span)?;
                    self.stack.push(ov);
                }
                Op::MakeClosure { params, chunk } => {
                    let fv = FuncValue {
                        params: params.clone(),
                        body: Expr::Block(Block {
                            stmts: Vec::new(),
                            span: Span::new(0, 0),
                        }),
                        env: Rc::clone(&interp.env),
                        chunk: Some(Rc::clone(chunk)),
                    };
                    self.stack.push(Value::Func(fv));
                }
                Op::MakeVariant {
                    name,
                    has_arg,
                    span,
                } => {
                    let av = if *has_arg {
                        Some(self.stack.pop().unwrap())
                    } else {
                        None
                    };
                    match (name.as_str(), av) {
                        ("ok", Some(v)) => self.stack.push(Value::Result(Ok(Box::new(v)))),
                        ("ok", None) => {
                            return Err(EvalError::new("`.ok` requires an argument", *span))
                        }
                        ("err", Some(v)) => self.stack.push(Value::Result(Err(Box::new(v)))),
                        ("err", None) => {
                            return Err(EvalError::new("`.err` requires an argument", *span))
                        }
                        ("some", Some(v)) => self.stack.push(Value::Option(Some(Box::new(v)))),
                        ("some", None) => {
                            return Err(EvalError::new("`.some` requires an argument", *span))
                        }
                        ("none", None) => self.stack.push(Value::Option(None)),
                        ("none", Some(_)) => {
                            return Err(EvalError::new("`.none` takes no argument", *span))
                        }
                        (other, _) => {
                            return Err(EvalError::new(
                                format!("unknown variant constructor `.{other}`"),
                                *span,
                            ))
                        }
                    }
                }
                Op::MatchArm { pat, next, has_env } => {
                    let sv = self.stack.pop().unwrap();
                    let matched = if *has_env {
                        let scope = Env::with_parent(&interp.env);
                        let m = interp.match_pattern(pat, &sv, &scope);
                        if m {
                            interp.env = scope;
                        }
                        m
                    } else {
                        interp.match_pattern(pat, &sv, &interp.env)
                    };
                    if !matched {
                        self.stack.push(sv);
                        self.frames.last_mut().unwrap().ip = *next;
                    }
                }
                Op::MatchError(span) => {
                    return Err(EvalError::new(
                        "non-exhaustive match: no arm matched",
                        *span,
                    ));
                }
                Op::IfLetMatch { pat, els, has_env } => {
                    let v = self.stack.pop().unwrap();
                    let matched = if *has_env {
                        let scope = Env::with_parent(&interp.env);
                        let m = interp.match_pattern(pat, &v, &scope);
                        if m {
                            interp.env = scope;
                        }
                        m
                    } else {
                        interp.match_pattern(pat, &v, &interp.env)
                    };
                    if !matched {
                        self.stack.push(v);
                        self.frames.last_mut().unwrap().ip = *els;
                    }
                }
                Op::TryOp(span) => {
                    let v = self.stack.pop().unwrap();
                    match v {
                        Value::Option(Some(inner)) => self.stack.push(*inner),
                        Value::Option(None) => {
                            match self.unwind_frame(Flow::Return(Value::Option(None)), interp) {
                                Unwind::Continue => {}
                                Unwind::Escaped(flow) => return Ok(flow),
                                Unwind::Error(e) => return Err(e),
                            }
                        }
                        Value::Result(Ok(inner)) => self.stack.push(*inner),
                        Value::Result(Err(e)) => {
                            match self.unwind_frame(Flow::Return(Value::Result(Err(e))), interp) {
                                Unwind::Continue => {}
                                Unwind::Escaped(flow) => return Ok(flow),
                                Unwind::Error(e) => return Err(e),
                            }
                        }
                        other => {
                            return Err(EvalError::new(
                                format!("cannot use `?` on a value of type `{other}`"),
                                *span,
                            ))
                        }
                    }
                }
                Op::Elvis(_span) => {
                    let v = self.stack.pop().unwrap();
                    match v {
                        Value::Option(Some(inner)) => {
                            // Push: flag (deepest), unwrapped, then right is
                            // compiled on top.  ElvisResult pops success, right,
                            // left — so flag must be deepest of the three.
                            self.stack.push(Value::Bool(true));
                            self.stack.push(*inner);
                        }
                        Value::Option(None) => {
                            // Flag false + Unit placeholder for the inner slot
                            // so ElvisResult still pops 3 and pushes 1.
                            self.stack.push(Value::Bool(false));
                            self.stack.push(Value::Unit);
                        }
                        other => {
                            // Non-Option left: pass through (allows chaining).
                            self.stack.push(Value::Bool(true));
                            self.stack.push(other);
                        }
                    }
                }
                Op::ElvisResult => {
                    // Stack (bottom→top): flag, inner/placeholder, right.
                    let right_val = self.stack.pop().unwrap();
                    let inner_val = self.stack.pop().unwrap();
                    let flag = self.stack.pop().unwrap();
                    match flag {
                        Value::Bool(true) => self.stack.push(inner_val),
                        _ => self.stack.push(right_val),
                    }
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
                Op::CallMethod { name, argc, span } => {
                    let argc = *argc;
                    let span = *span;
                    let mut args = Vec::with_capacity(argc as usize);
                    for _ in 0..argc {
                        args.push(self.stack.pop().unwrap());
                    }
                    args.reverse();
                    let recv = self.stack.pop().unwrap();
                    // A func-valued field is a direct call; otherwise the
                    // last component is a method on the receiver.
                    match Interp::object_field(&recv, name, span) {
                        Ok(f) => self.call_value(f, args, span, interp)?,
                        Err(_) => {
                            let f = interp.lookup_method(&recv, name, span)?;
                            let mut arg_vals = vec![recv];
                            arg_vals.extend(args);
                            self.call_value(f, arg_vals, span, interp)?;
                        }
                    }
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
                Op::FormatValue(span) => {
                    let spec = self.stack.pop().unwrap();
                    let val = self.stack.pop().unwrap();
                    let spec_str = match spec {
                        Value::Str(s) => s,
                        _ => {
                            return Err(EvalError::new(
                                "format spec must be a string".to_string(),
                                *span,
                            ))
                        }
                    };
                    let formatted = crate::interp::format_value_with_spec(&val, &spec_str);
                    self.stack.push(Value::Str(formatted));
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
                Op::PopN(n) => {
                    let len = self.stack.len();
                    let result = self.stack.pop().unwrap();
                    self.stack.truncate(len - 1 - *n as usize);
                    self.stack.push(result);
                }
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
                // Arguments stay on the stack as the callee's parameter
                // slots. The callee runs in its captured environment; the
                // body's own scope (if any) is entered by its first op.
                let stack_base = self.stack.len();
                self.stack.extend(args);
                let prev_env = std::mem::replace(&mut interp.env, Rc::clone(&fv.env));
                self.frames.push(Frame {
                    chunk: fv.chunk.unwrap(),
                    ip: 0,
                    prev_env,
                    stack_base,
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
    fn vm_nested_path_assignment_keeps_shape() {
        // `r.p.x = 9` must mutate only the innermost field: `r` stays a
        // `Rect`, its other fields are untouched.
        for src in [
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.p.x",
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.w",
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.p.y",
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = r.p.x + 8\nr.p.x",
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nfunc f(r: Rect) -> int { r.p.x = 9\nr.p.x + r.w }\nf(Rect{ p: Point{ x: 1, y: 2 }, w: 3 })",
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nfunc f(r: Rect) -> int { r.p.x = 9\nr.p.x }\nf(Rect{ p: Point{ x: 1, y: 2 }, w: 3 })",
            "struct A { b: B }\nstruct B { c: C }\nstruct C { v: int }\na := A{ b: B{ c: C{ v: 1 } } }\na.b.c.v = 42\na.b.c.v",
            "struct A { b: B }\nstruct B { c: C }\nstruct C { v: int }\nfunc f(a: A) -> int { a.b.c.v = 42\na.b.c.v }\nf(A{ b: B{ c: C{ v: 1 } } })",
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.p.x + r.p.y + r.w",
        ] {
            assert_same(src);
        }
        // The root keeps its shape: `r` is still a `Rect` after mutation.
        assert_eq!(
            run_src(
                "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.w"
            )
            .unwrap(),
            Value::Int(3)
        );
    }

    #[test]
    fn vm_slot_locals_match_tree_walker() {
        // Slot-based locals: shadowing, nested scopes, params, and locals
        // inside match arms / if-let branches must agree with the
        // environment-based tree-walker.
        for src in [
            // ---- shadowing and nested scopes ----
            "x := 1\n{ let x = 2\nx }\nx",
            "x := 1\n{ let y = 2\n{ let z = 3\nx + y + z } }",
            "x := 1\n{ let x = 2\n{ let x = 3\nx } }",
            "x := 1\nlet y = 2\n{ let y = 3\ny }\ny",
            "func f() -> int { let a = 1\nlet b = 2\nlet c = a + b\nc }\nf()",
            "func f(n: int) -> int { let m = n * 2\nm + n }\nf(5)",
            "func f(n: int) -> int { let n = n + 1\nn }\nf(5)",
            "func f() -> int { let x = 1\n{ let x = 2\nx }\nx }\nf()",
            // ---- locals in match arms / if-let branches ----
            "match 5 { n => { let m = n * 2\nm } }",
            "match 5 { 1 => 10, n => { let m = n * 2\nm } }",
            "x := .some(5)\nif let .some(n) = x { let m = n + 1\nm } else { 0 }",
            "x := .none\nif let .some(n) = x { let m = n + 1\nm } else { 0 }",
            "x := .some(5)\nif let .some(n) = x { let m = n + 1\nm }",
            // ---- struct fields on slot params / receivers ----
            "struct Point { x: int }\nfunc f(p: Point) -> int { p.x }\nf(Point{ x: 7 })",
            "struct Point { x: int }\nfunc dist(p: Point) -> int { p.x }\np := Point{ x: 9 }\np.dist()",
            "struct Point { x: int }\nstruct Holder { p: Point }\nfunc dist(p: Point) -> int { p.x }\nh := Holder{ p: Point{ x: 9 } }\nh.p.dist()",
            "struct Point { x: int }\nfunc f(p: Point) -> int { p.x = 5\np.x }\nf(Point{ x: 1 })",
            // ---- closures capturing locals ----
            "func outer() { let x = 10\n|x| x + 1 }\ng := outer()\ng(5)",
            "func outer() { let x = 10\nlet y = 20\n|x| x + y }\ng := outer()\ng(5)",
            "func outer() { let x = 10\n{ let y = x + 1\ny } }\ng := outer()\ng(5)",
            "func counter() { let n = 0\n|inc| { n = n + inc\nn } }\nc := counter()\nc(1)\nc(2)",
            // ---- loop var does not leak ----
            "x := 0\nfor x in 0..3 { x }\nx",
            "x := 5\nfor i in 0..3 { let x = i\nx }\nx",
            "sum := 0\nfor i in 0..3 { let j = i * 2\nsum = sum + j }\nsum",
        ] {
            assert_same(src);
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
            // ---- native closures ----
            "f := |x| x * 2\nf(5)",
            "f := |x, y| x + y\nf(2, 3)",
            "n := 10\nf := |x| x + n\nf(5)",
            "f := |x| |y| x + y\nf(2)(3)",
            "f := |x| { let y = x * 2\ny }\nf(5)",
            "f := |n| if n < 2 { n } else { f(n - 1) + f(n - 2) }\nf(5)",
            "func apply(f, x) { f(x) }\napply(|n| n + 1, 41)",
            "func outer() { func inner(n: int) -> int { n * 3 }\ninner }\ng := outer()\ng(7)",
            // ---- native variants ----
            ".ok(5)",
            ".err(\"boom\")",
            ".some(1)",
            ".none",
            "x := .ok(5)\nmatch x { .ok(n) => n, .err(e) => e }",
            "x := .err(\"boom\")\nmatch x { .ok(n) => n, .err(e) => e }",
            "x := .some(1)\nmatch x { .some(n) => n, .none => 0 }",
            "x := .none\nmatch x { .some(n) => n, .none => 0 }",
            // ---- native match ----
            "match 5 { 5 => \"five\", _ => \"other\" }",
            "match 3 { 5 => \"five\", _ => \"other\" }",
            "match 5 { n => n * 2 }",
            "match 5 { 1 => 10, 2 => 20, n => n }",
            "match true { true => 1, false => 0 }",
            "match 1.5 { 1.5 => \"one five\", _ => \"other\" }",
            "match .some(1) { .some(n) => n + 1, .none => 0 }",
            "match .some(.ok(2)) { .some(.ok(n)) => n, _ => 0 }",
            "match .none { .some(n) => n, .none => 0 }",
            "match .err(9) { .ok(n) => n, .err(e) => e }",
            "match 5 { n => { let m = n * 2\nm } }",
            "match 5 { 1 => 10, _ => 20 }",
            "match 5 { 5 => 1, 5 => 2, _ => 3 }",
            // ---- native if-let ----
            "x := .some(5)\nif let .some(n) = x { n } else { 0 }",
            "x := .none\nif let .some(n) = x { n } else { 0 }",
            "x := .ok(5)\nif let .ok(n) = x { n } else { 0 }",
            "x := .err(7)\nif let .ok(n) = x { n } else { 0 }",
            "x := .some(5)\nif let .some(n) = x { n }",
            "x := .none\nif let .some(n) = x { n }",
            "x := 42\nif let n = x { n } else { 0 }",
            // ---- native try ----
            "func f() -> result<int, str> { let x = .ok(5)\nx? }\nf()",
            "func f() -> result<int, str> { let x = .err(\"no\")\nx? }\nf()",
            "func f() -> option<int> { let x = .some(3)\nx? }\nf()",
            "func f() -> option<int> { let x = .none\nx? }\nf()",
            "func f() -> result<int, str> { let x = .ok(5)\nlet y = .ok(6)\nx? + y? }\nf()",
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
