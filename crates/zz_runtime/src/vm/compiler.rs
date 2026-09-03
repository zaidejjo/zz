use std::rc::Rc;

use zz_frontend::ast::{BinOp, Block, Expr, FmtPart, Param, Pattern, Program, Stmt};
use zz_frontend::span::Span;

use super::capture::*;
use super::chunk::Chunk;
use super::op::Op;
use crate::value::Value;

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

/// Compiled info for a known function, used to reorder named args
/// and fill defaults at call sites.
#[derive(Debug, Clone)]
struct FuncInfo {
    param_names: Vec<String>,
    has_default: Vec<bool>,
    defaults: Vec<Option<Box<Expr>>>,
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
    /// Known function signatures for named-arg reordering.
    func_info: std::collections::HashMap<String, FuncInfo>,
}

enum JumpKind {
    Always,
    IfFalse,
    IfTrue,
    IfFalseBool(Span),
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
            func_info: std::collections::HashMap::new(),
        }
    }

    /// Compile a whole program. The top level runs in the interpreter's root
    /// scope (no `EnterScope`), matching the tree-walker.
    pub fn compile_program(program: &Program) -> Chunk {
        let mut c = Compiler::new();
        c.is_main = true;
        for stmt in &program.stmts {
            if let Stmt::Func { name, params, .. } = stmt {
                let full = name.join(".");
                c.func_info.insert(
                    full,
                    FuncInfo {
                        param_names: params.iter().map(|p| p.name.name.clone()).collect(),
                        has_default: params.iter().map(|p| p.default.is_some()).collect(),
                        defaults: params.iter().map(|p| p.default.clone()).collect(),
                    },
                );
            }
        }
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
        let span = match &op {
            Op::Break(span) | Op::Continue(span) => *span,
            Op::BinOp(_, span) | Op::UnOp(_, span) => *span,
            Op::LoadVar(_, span) | Op::StoreVar(_, span) => *span,
            Op::LoadPath(_, span) | Op::StorePath(_, span) => *span,
            Op::JumpIfFalseBool(_, span) => *span,
            Op::ForSetup { span, .. } | Op::WhileCond { span, .. } => *span,
            Op::ArrayPush(span) | Op::IndexOp(span) | Op::StoreIndexOp(span) => *span,
            Op::SliceOp(span) | Op::MakeRange(span) => *span,
            Op::MakeStruct { span, .. } | Op::GetField(_, span) | Op::SetField(_, span) => *span,
            Op::MakeVariant { span, .. } | Op::MatchError(span) => *span,
            Op::TryOp(span) | Op::Elvis(span) => *span,
            Op::Call { span, .. } | Op::CallPath { span, .. } | Op::CallMethod { span, .. } => {
                *span
            }
            Op::FormatValue(span) => *span,
            _ => Span::default(),
        };
        let effect = Self::stack_effect(&op);
        self.stack_height = self.stack_height.saturating_add_signed(effect as isize);
        self.chunk.spans.push(span);
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
            Op::SlotAddInt { .. } => 0,
            Op::MakeFunc { .. } | Op::RegisterStruct { .. } | Op::MakeClosure { .. } => 1,
            Op::BinOp(..) => -1,
            Op::UnOp(..) => 0,
            Op::Jump(_) => 0,
            Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::JumpIfFalseBool(..) => -1,
            Op::Return => -1,
            Op::ForSetup { num_vars, .. } => 1 + *num_vars as i64,
            Op::ForNext { .. } => 0,
            Op::WhileSetup { .. } => 0,
            Op::WhileCond { .. } => -1,
            Op::Break(_) | Op::Continue(_) => 0,
            Op::SetLoopResult => -1,
            Op::MakeArray(n) => 1 - *n as i64,
            Op::UnpackTuple(n) => *n as i64 - 1,
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
            Op::MatchGuard { .. } => -1,
            Op::MatchError(_) => 0,
            Op::IfLetMatch { .. } => -1,
            Op::TryOp(_) => 0,
            Op::Elvis(_) => 1,
            Op::ElvisResult => -2,
            Op::Call { argc, .. } | Op::CallMethod { argc, .. } => -(*argc as i64),
            Op::CallPath { argc, .. } => 1 - (*argc as i64),
            Op::Concat(n) => 1 - *n as i64,
            Op::FormatValue(_) => -1,
            Op::EnterScope | Op::ExitScope => 0,
            Op::PopN(n) => -(*n as i64),
            Op::DeferRecord => -1,
        }
    }

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
            let slot = self.stack_height - 1;
            self.locals.push(Local {
                name: name.to_string(),
                slot,
                in_env: false,
            });
            true
        }
    }

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

    /// Match `x = x + y` / `x = y + x` where `x` and `y` both resolve to
    /// local slots. Returns `(dst, src)` so the VM can fuse the load/add/
    /// store into a single in-place `SlotAddInt`.
    fn try_slot_add(&self, target: &str, value: &Expr) -> Option<(u16, u16)> {
        let Expr::Binary {
            op: binop,
            left,
            right,
            ..
        } = value
        else {
            return None;
        };
        if *binop != zz_frontend::ast::BinOp::Add {
            return None;
        }
        let dst = match self.resolve(target) {
            Resolved::Slot(slot) => slot as u16,
            Resolved::Env => return None,
        };
        let src = match (left.as_ref(), right.as_ref()) {
            (Expr::Ident { name: ln, .. }, Expr::Ident { name: rn, .. }) => {
                // `x = x + y` -> dst + src, or `x = y + x` -> dst + src
                if ln == target {
                    match self.resolve(rn) {
                        Resolved::Slot(slot) => slot as u16,
                        Resolved::Env => return None,
                    }
                } else if rn == target {
                    match self.resolve(ln) {
                        Resolved::Slot(slot) => slot as u16,
                        Resolved::Env => return None,
                    }
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        Some((dst, src))
    }

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

    fn compile_path_store(&mut self, parts: &[String], span: Span) {
        if let Resolved::Slot(slot) = self.resolve(&parts[0]) {
            self.emit(Op::LoadSlot(slot as u16));
            for part in &parts[1..parts.len() - 1] {
                self.emit(Op::GetField(part.clone(), span));
            }
            self.emit(Op::SetField(parts.last().unwrap().clone(), span));
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
        let (op, span) = match kind {
            JumpKind::Always => (Op::Jump(0), Span::default()),
            JumpKind::IfFalse => (Op::JumpIfFalse(0), Span::default()),
            JumpKind::IfTrue => (Op::JumpIfTrue(0), Span::default()),
            JumpKind::IfFalseBool(span) => (Op::JumpIfFalseBool(0, span), span),
        };
        self.chunk.spans.push(span);
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
            Op::ForNext { vars, in_env, .. } => Op::ForNext {
                vars,
                exit: target,
                in_env,
            },
            Op::WhileCond { span, .. } => Op::WhileCond { exit: target, span },
            other => panic!("patch_jump on non-jump op: {other:?}"),
        };
    }

    fn emit_for_next(&mut self, vars: Vec<String>, in_env: bool) -> usize {
        let pos = self.chunk.code.len();
        self.emit(Op::ForNext {
            vars,
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

    /// Compile a destructuring pattern. Expects the value to be on the stack.
    fn compile_destructure(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Wildcard { .. } => {
                self.emit(Op::Pop);
            }
            Pattern::Binding { name } => {
                if self.declare_local(&name.name) {
                    // Local already declared, value stays on stack
                } else {
                    self.emit(Op::Pop);
                }
            }
            Pattern::Tuple { pats, .. } => {
                // Value is on stack. We need to unpack it.
                // Emit UnpackTuple to split into individual elements.
                self.emit(Op::UnpackTuple(pats.len() as u8));
                // After UnpackTuple, elements are in reverse order on stack:
                // [last, ..., second, first] where first is on top.
                // Declare locals in forward order so first name gets the
                // slot for the top-of-stack element.
                for pat in pats {
                    self.compile_destructure(pat);
                }
            }
            Pattern::Literal { .. } | Pattern::Variant { .. } => {
                // These are match-only patterns, not valid in destructuring
                self.emit(Op::Pop);
            }
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
            Stmt::Import { .. } => StmtValue::None,
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
            Stmt::Impl { name, methods, .. } => {
                let type_name = name.join(".");
                for method in methods {
                    if let Stmt::Func {
                        name: mname,
                        generics: _,
                        params,
                        ret: _,
                        body,
                        ..
                    } = method
                    {
                        let full_name = format!("{}.{}", type_name, mname.join("."));
                        let chunk = self.compile_func_body(body, params);
                        self.emit(Op::MakeFunc {
                            name: full_name,
                            params: params.clone(),
                            chunk,
                        });
                    }
                }
                StmtValue::Discard
            }
            Stmt::For {
                vars,
                iter,
                body,
                span,
            } => {
                let pre = self.stack_height;
                let num_vars_u8 = vars.len() as u8;
                self.emit_const(Value::Unit);
                self.compile_expr(iter);
                let setup_pos = self.chunk.code.len();
                self.emit(Op::ForSetup {
                    exit: 0,
                    header: 0,
                    span: *span,
                    num_vars: num_vars_u8,
                });
                let header = self.chunk.code.len();
                // Determine if any var is captured by an inner closure
                let any_captured = vars.iter().any(|v| self.captured.contains(&v.name));
                let var_names: Vec<String> = vars.iter().map(|v| v.name.clone()).collect();
                let j = self.emit_for_next(var_names.clone(), any_captured);
                // Push locals for each var — last var at highest slot,
                // first at lowest
                let num_vars = vars.len();
                for (i, v) in vars.iter().enumerate().rev() {
                    let in_env = self.captured.contains(&v.name);
                    self.locals.push(Local {
                        name: v.name.clone(),
                        slot: self.stack_height - num_vars + i,
                        in_env,
                    });
                }
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
                    num_vars: num_vars_u8,
                };
                // Pop locals
                for _ in vars {
                    self.locals.pop();
                }
                self.stack_height = pre + 1;
                StmtValue::Discard
            }
            Stmt::Break { span } => {
                self.emit(Op::Break(*span));
                StmtValue::None
            }
            Stmt::Continue { span } => {
                self.emit(Op::Continue(*span));
                StmtValue::None
            }
            Stmt::Defer { expr, .. } => {
                let body = Block {
                    stmts: vec![Stmt::Expr(expr.as_ref().clone())],
                    span: expr.span(),
                };
                let chunk = self.compile_func_body(&body, &[]);
                self.emit(Op::MakeClosure {
                    params: vec![],
                    chunk,
                });
                self.emit(Op::DeferRecord);
                StmtValue::None
            }
            Stmt::Destructure { pat, value, .. } => {
                self.compile_expr(value);
                self.compile_destructure(pat);
                StmtValue::None
            }
            Stmt::Assign { target, value, .. } => match target {
                Expr::Ident { name, span } => {
                    // Fast path: `x = x + y` / `x = y + x` with both operands
                    // resolving to local slots -> single in-place int add.
                    if let Some((dst, src)) = self.try_slot_add(name, value) {
                        self.emit(Op::SlotAddInt { dst, src });
                        self.emit_const(Value::Unit);
                        StmtValue::Discard
                    } else {
                        self.compile_expr(value);
                        match self.resolve(name) {
                            Resolved::Slot(slot) => self.emit(Op::StoreSlot(slot as u16)),
                            Resolved::Env => self.emit(Op::StoreVar(name.clone(), *span)),
                        }
                        self.emit_const(Value::Unit);
                        StmtValue::Discard
                    }
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
                    self.compile_write_back(obj);
                    self.emit_const(Value::Unit);
                    StmtValue::Discard
                }
                _ => unreachable!("unhandled assignment target"),
            },
            Stmt::Expr(e) => {
                // Method call write-back: if `obj.method(args)` is called as a
                // statement, the return value (e.g. the new array from push/pop)
                // must be written back to `obj` so the mutation is visible.
                // Only intercept known mutating methods to avoid corrupting
                // non-mutating calls (e.g. `arr.len()` should not write back).
                if let Expr::Call {
                    callee,
                    args,
                    named,
                    ..
                } = e
                {
                    // Known mutating array methods that return the modified array.
                    const MUTATING_METHODS: &[&str] = &[
                        "push", "pop", "insert", "remove", "reverse", "sort", "append",
                    ];

                    // Handle `arr.push(x)` — parsed as Path { parts: ["arr", "push"] }
                    if let Expr::Path { parts, .. } = callee.as_ref() {
                        if parts.len() == 2 {
                            let method_name = &parts[1];
                            if MUTATING_METHODS.contains(&method_name.as_str()) {
                                let obj_name = &parts[0];
                                // Compile: LoadVar(obj) + args + CallMethod(method)
                                match self.resolve(obj_name) {
                                    Resolved::Slot(slot) => self.emit(Op::LoadSlot(slot as u16)),
                                    Resolved::Env => {
                                        self.emit(Op::LoadVar(obj_name.clone(), e.span()))
                                    }
                                }
                                for a in args {
                                    self.compile_expr(a);
                                }
                                for (_, val) in named {
                                    self.compile_expr(val);
                                }
                                self.emit(Op::CallMethod {
                                    name: method_name.clone(),
                                    argc: (args.len() + named.len()) as u16,
                                    span: e.span(),
                                });
                                // Write back: StoreVar(obj) pops the result into the variable
                                match self.resolve(obj_name) {
                                    Resolved::Slot(slot) => self.emit(Op::StoreSlot(slot as u16)),
                                    Resolved::Env => {
                                        self.emit(Op::StoreVar(obj_name.clone(), e.span()))
                                    }
                                }
                                return StmtValue::None;
                            }
                        }
                    }
                    // Handle `obj.method(x)` — parsed as Field { obj, name }
                    if let Expr::Field {
                        obj: field_obj,
                        name: method_name,
                        ..
                    } = callee.as_ref()
                    {
                        if MUTATING_METHODS.contains(&method_name.as_str()) {
                            if let Expr::Ident { name, span } = field_obj.as_ref() {
                                self.compile_expr(field_obj);
                                for a in args {
                                    self.compile_expr(a);
                                }
                                for (_, val) in named {
                                    self.compile_expr(val);
                                }
                                self.emit(Op::CallMethod {
                                    name: method_name.clone(),
                                    argc: (args.len() + named.len()) as u16,
                                    span: e.span(),
                                });
                                match self.resolve(name) {
                                    Resolved::Slot(slot) => self.emit(Op::StoreSlot(slot as u16)),
                                    Resolved::Env => self.emit(Op::StoreVar(name.clone(), *span)),
                                }
                                return StmtValue::None;
                            }
                        }
                    }
                }
                // Built-in `append(arr, val)` write-back: same as method call.
                // append returns the mutated array; store it back to arr.
                if let Expr::Call {
                    callee,
                    args,
                    named,
                    ..
                } = e
                {
                    if let Expr::Ident { name: fname, .. } = callee.as_ref() {
                        if fname == "append" && args.len() == 2 && named.is_empty() {
                            if let Expr::Ident {
                                name: arr_name,
                                span,
                            } = &args[0]
                            {
                                self.compile_expr(e);
                                match self.resolve(arr_name) {
                                    Resolved::Slot(slot) => self.emit(Op::StoreSlot(slot as u16)),
                                    Resolved::Env => {
                                        self.emit(Op::StoreVar(arr_name.clone(), *span))
                                    }
                                }
                                return StmtValue::None;
                            }
                        }
                    }
                }
                self.compile_expr(e);
                StmtValue::Discard
            }
        }
    }

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

    fn compile_block_body(&mut self, block: &Block) {
        let scope_base = self.locals.len();
        let mut last = StmtValue::None;
        for (i, stmt) in block.stmts.iter().enumerate() {
            last = self.compile_stmt(stmt);
            if i < block.stmts.len() - 1 && matches!(last, StmtValue::Discard) {
                self.emit(Op::Pop);
            }
        }
        if block.stmts.is_empty() || matches!(last, StmtValue::None) {
            self.emit_const(Value::Unit);
        }
        let n = self.locals[scope_base..]
            .iter()
            .filter(|l| !l.in_env)
            .count();
        if n > 0 {
            self.emit(Op::PopN(n as u16));
        }
        self.locals.truncate(scope_base);
    }

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

    fn compile_reordered_args(&mut self, func_name: &str, args: &[Expr], named: &[(String, Expr)]) {
        let info = match self.func_info.get(func_name) {
            Some(fi) => fi.clone(),
            None => {
                for a in args {
                    self.compile_expr(a);
                }
                for (_, val) in named {
                    self.compile_expr(val);
                }
                return;
            }
        };
        let n = info.param_names.len();
        let mut slots: Vec<Option<Expr>> = vec![None; n];
        for (i, arg) in args.iter().enumerate() {
            if i < n {
                slots[i] = Some(arg.clone());
            }
        }
        for (name, val) in named {
            if let Some(i) = info.param_names.iter().position(|pn| pn == name) {
                if slots[i].is_none() {
                    slots[i] = Some(val.clone());
                }
            }
        }
        for (i, slot) in slots.iter_mut().enumerate() {
            if slot.is_none() {
                if let Some(Some(default)) = info.defaults.get(i) {
                    *slot = Some(default.as_ref().clone());
                }
            }
        }
        for slot in slots.iter().flatten() {
            self.compile_expr(slot);
        }
    }

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
            Expr::Str { value, .. } => self.emit_const(Value::Str(value.clone().into())),
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
            Expr::Call {
                callee,
                args,
                named,
                span,
            } => match callee.as_ref() {
                Expr::Path { parts, span: pspan } => {
                    let func_name = parts.join(".");
                    let is_input = parts.len() == 1
                        && parts[0] == "input"
                        && args.is_empty()
                        && named.is_empty();
                    let is_range = parts.len() == 1
                        && parts[0] == "range"
                        && args.len() + named.len() < 3
                        && named.is_empty();

                    let has_named_or_defaults = !named.is_empty()
                        || self
                            .func_info
                            .get(&func_name)
                            .is_some_and(|fi| fi.has_default.iter().any(|&d| d));

                    let argc = if is_input {
                        1
                    } else if is_range {
                        3
                    } else if has_named_or_defaults {
                        self.func_info
                            .get(&func_name)
                            .map_or(args.len() + named.len(), |fi| fi.param_names.len())
                    } else {
                        args.len() + named.len()
                    };

                    if let Resolved::Slot(slot) = self.resolve(&parts[0]) {
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
                        } else if has_named_or_defaults {
                            self.compile_reordered_args(&func_name, args, named);
                        } else {
                            for a in args {
                                self.compile_expr(a);
                            }
                        }
                        if is_input {
                            self.emit_const(Value::Str(String::new().into()));
                        }
                        self.emit(Op::CallMethod {
                            name: parts.last().unwrap().clone(),
                            argc: argc as u16,
                            span: *span,
                        });
                    } else {
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
                        } else if has_named_or_defaults {
                            self.compile_reordered_args(&func_name, args, named);
                        } else {
                            for a in args {
                                self.compile_expr(a);
                            }
                        }
                        if is_input {
                            self.emit_const(Value::Str(String::new().into()));
                        }
                        self.emit(Op::CallPath {
                            parts: parts.clone(),
                            argc: argc as u16,
                            span: *span,
                            pspan: *pspan,
                        });
                    }
                }
                Expr::Field { obj, name, span: _ } => {
                    self.compile_expr(obj);
                    for a in args {
                        self.compile_expr(a);
                    }
                    for (_, val) in named {
                        self.compile_expr(val);
                    }
                    self.emit(Op::CallMethod {
                        name: name.clone(),
                        argc: (args.len() + named.len()) as u16,
                        span: *span,
                    });
                }
                _ => {
                    let is_input = matches!(callee.as_ref(), Expr::Ident { name, .. } if name == "input")
                        && args.is_empty()
                        && named.is_empty();
                    let is_range = matches!(callee.as_ref(), Expr::Ident { name, .. } if name == "range")
                        && args.len() + named.len() < 3
                        && named.is_empty();

                    let callee_name = match callee.as_ref() {
                        Expr::Ident { name, .. } => Some(name.as_str()),
                        Expr::Path { parts, .. } => Some(parts[0].as_str()),
                        _ => None,
                    };
                    let has_named_or_defaults = callee_name.is_some_and(|cn| {
                        !named.is_empty()
                            || self
                                .func_info
                                .get(cn)
                                .is_some_and(|fi| fi.has_default.iter().any(|&d| d))
                    });

                    let argc = if is_input {
                        1
                    } else if is_range {
                        3
                    } else if has_named_or_defaults {
                        callee_name
                            .and_then(|cn| self.func_info.get(cn))
                            .map_or(args.len() + named.len(), |fi| fi.param_names.len())
                    } else {
                        args.len() + named.len()
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
                    } else if has_named_or_defaults {
                        self.compile_reordered_args(callee_name.unwrap(), args, named);
                    } else {
                        for a in args {
                            self.compile_expr(a);
                        }
                    }
                    if is_input {
                        self.emit_const(Value::Str(String::new().into()));
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
                            self.emit_const(Value::Str(t.clone().into()));
                            n += 1;
                        }
                        FmtPart::Expr(e, Some(spec)) => {
                            self.compile_expr(e);
                            self.emit_const(Value::Str(spec.clone().into()));
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
                self.stack_height = pre + 1;
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.compile_expr(e);
                }
                self.emit(Op::MakeArray(elems.len() as u16));
            }
            Expr::Tuple { items, .. } => {
                for e in items {
                    self.compile_expr(e);
                }
                self.emit(Op::MakeArray(items.len() as u16));
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                span,
            } => {
                let result_slot = self.stack_height;
                self.emit_const(Value::Unit);
                self.emit(Op::MakeArray(0));
                self.emit(Op::StoreSlot(result_slot as u16));
                self.compile_expr(iter);
                let setup_pos = self.chunk.code.len();
                self.emit(Op::ForSetup {
                    exit: 0,
                    header: 0,
                    span: *span,
                    num_vars: 1,
                });
                let header = self.chunk.code.len();
                let in_env = self.captured.contains(&var.name);
                let j = self.emit_for_next(vec![var.name.clone()], in_env);
                self.locals.push(Local {
                    name: var.name.clone(),
                    slot: self.stack_height - 1,
                    in_env,
                });
                if let Some(f) = filter {
                    self.compile_expr(f);
                    self.emit(Op::JumpIfFalse(header));
                    self.emit(Op::LoadSlot(result_slot as u16));
                    self.compile_expr(body);
                    self.emit(Op::ArrayPush(*span));
                    self.emit(Op::StoreSlot(result_slot as u16));
                    self.emit(Op::Jump(header));
                } else {
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
                    num_vars: 1,
                };
                self.locals.pop();
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
                // Save scrutinee in a slot. When arms have guards, each
                // arm reloads from the slot (no push-back on miss). Without
                // guards, the original single-copy approach works.
                let has_guards = arms.iter().any(|a| a.guard.is_some());
                if has_guards {
                    self.compile_expr(scrutinee);
                    // Store scrutinee in a temporary variable, then pop
                    // the stack copy (only env copy remains for reload).
                    let tmp_name = format!("__match_scrutinee_{}", self.chunk.code.len());
                    self.emit(Op::DefineVar(tmp_name.clone()));
                    self.emit(Op::Pop);
                    let mut arm_positions = Vec::with_capacity(arms.len());
                    let mut body_jumps = Vec::with_capacity(arms.len());
                    let mut guard_positions: Vec<usize> = Vec::new();
                    for arm in arms {
                        let has_env = pattern_binds(&arm.pat);
                        self.emit(Op::LoadVar(tmp_name.clone(), *span));
                        let pos = self.chunk.code.len();
                        self.emit(Op::MatchArm {
                            pat: arm.pat.clone(),
                            next: 0,
                            has_env,
                            restore: false,
                        });
                        // Compile guard if present
                        if let Some(guard) = &arm.guard {
                            guard_positions.push(pos);
                            self.compile_expr(guard);
                            let guard_pos = self.chunk.code.len();
                            self.emit(Op::MatchGuard {
                                next: 0,
                                has_env: pattern_binds(&arm.pat),
                            });
                            guard_positions.push(guard_pos);
                        }
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
                    // Patch arm jumps
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
                            restore: false,
                        };
                    }
                    // Patch guard jumps — jump to the LoadVar before the next
                    // arm's MatchArm (arm_positions[i+1] - 1).
                    let mut gi = 0;
                    for (i, arm) in arms.iter().enumerate() {
                        if arm.guard.is_some() {
                            let guard_jmp_pos = guard_positions[gi + 1];
                            let next = if i + 1 < arms.len() {
                                arm_positions[i + 1] - 1 // LoadVar before MatchArm
                            } else {
                                error_pos
                            };
                            self.chunk.code[guard_jmp_pos] = Op::MatchGuard {
                                next,
                                has_env: pattern_binds(&arms[i].pat),
                            };
                            gi += 2;
                        }
                    }
                    for j in body_jumps {
                        self.patch_jump(j);
                    }
                } else {
                    // No guards: original single-copy approach
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
                            restore: true,
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
                            restore: true,
                        };
                    }
                    for j in body_jumps {
                        self.patch_jump(j);
                    }
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
