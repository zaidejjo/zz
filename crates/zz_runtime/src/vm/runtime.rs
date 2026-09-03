use std::cell::RefCell;
use std::rc::Rc;

use zz_frontend::ast::{Block, Expr};
use zz_frontend::span::Span;

use super::chunk::Chunk;
use super::op::Op;
use crate::env::Env;
use crate::eval::{EvalError, Interp};
use crate::runtime::ops::{
    eval_binary, eval_unary, get_index, object_field, set_index, set_object_field, slice_value,
};
use crate::runtime::Flow;
use crate::value::{FuncValue, NativeFunc, ObjectValue, RangeValue, Value};

/// One active call frame.
struct Frame {
    chunk: Rc<Chunk>,
    ip: usize,
    /// Environment to restore when this frame returns.
    prev_env: Rc<RefCell<Env>>,
    /// Stack index where this frame's evaluation begins.
    stack_base: usize,
    /// Deferred closures accumulated in this frame. Saved/restored across
    /// nested calls so each frame only drains its own defers.
    defer_stack: Vec<Value>,
    /// Function name for backtraces (empty string for top-level).
    func_name: String,
    /// Source span of the function definition for backtraces.
    func_span: Span,
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
    /// Deferred closures for the current frame. `DeferRecord` pushes here;
    /// `Return` and frame-end pop and execute in LIFO order.
    defer_stack: Vec<Value>,
    /// When executing defers before a return, this holds the saved return
    /// value and remaining deferred closures. `None` when not in a defer-
    /// execution sequence.
    defer_return: Option<DeferReturn>,
}

/// State saved during defer-before-return execution.
struct DeferReturn {
    return_value: Value,
    remaining: Vec<Value>,
    /// Parent frame's deferred closures, saved so they can be restored
    /// after this frame's defers complete.
    parent_defers: Vec<Value>,
    /// True if this defer sequence was triggered by an explicit `Return`
    /// (as opposed to chunk-end implicit return). When all defers finish,
    /// a Return-origin defer must unwind the current frame rather than
    /// just pushing the return value and continuing.
    from_return: bool,
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
            defer_stack: Vec::new(),
            defer_return: None,
        }
    }

    /// Push a value onto the VM stack. Used by `Interp::call_func` to set up
    /// compiled closure parameters before calling `run_chunk_with_base`.
    pub(crate) fn push(&mut self, v: Value) {
        self.stack.push(v);
    }

    /// Push a deferred closure's chunk as a new frame for inline execution.
    fn push_defer_frame(&mut self, interp: &mut Interp) {
        let state = self.defer_return.as_mut().unwrap();
        let closure = state.remaining.pop().unwrap();
        if let Value::Func(fv) = closure {
            if let Some(chunk) = fv.chunk {
                let stack_base = self.stack.len();
                let prev_env = std::mem::replace(&mut interp.env, Rc::clone(&fv.env));
                let saved_defers = std::mem::take(&mut self.defer_stack);
                self.frames.push(Frame {
                    chunk,
                    ip: 0,
                    prev_env,
                    stack_base,
                    defer_stack: saved_defers,
                    func_name: String::new(),
                    func_span: Span::default(),
                });
            }
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
            defer_stack: Vec::new(),
            func_name: String::new(),
            func_span: Span::default(),
        });

        // Cache chunk pointers locally to avoid re-fetching from frames on
        // every instruction.  `ip` stays in a register; we only sync it back
        // to the Frame struct at frame-change points (Call/Return/defer).
        let mut cached_code: *const Vec<Op> = {
            let f = self.frames.last().unwrap();
            let c = unsafe { &*Rc::as_ptr(&f.chunk) };
            &c.code
        };
        let mut cached_constants: *const Vec<Value> = {
            let f = self.frames.last().unwrap();
            let c = unsafe { &*Rc::as_ptr(&f.chunk) };
            &c.constants
        };
        let mut ip: usize = 0;

        // Re-cache from the current top frame (after any frame push/pop).
        macro_rules! re_cache {
            () => {{
                let f = self.frames.last().unwrap();
                let c = unsafe { &*Rc::as_ptr(&f.chunk) };
                cached_code = &c.code;
                cached_constants = &c.constants;
                ip = f.ip;
            }};
        }

        loop {
            // SAFETY: cached_code/cached_constants point into the current
            // frame's Chunk which is kept alive by the Rc in self.frames.
            let code: &Vec<Op> = unsafe { &*cached_code };
            let constants: &Vec<Value> = unsafe { &*cached_constants };

            if ip >= code.len() {
                let sb = self.frames.last().unwrap().stack_base;
                let v = if self.stack.len() > sb {
                    self.stack.pop().unwrap()
                } else {
                    Value::Unit
                };
                let f = self.frames.pop().unwrap();
                self.stack.truncate(f.stack_base);
                interp.env = f.prev_env;
                let parent_defers = f.defer_stack;

                if let Some(ref mut state) = self.defer_return {
                    if !state.remaining.is_empty() {
                        self.push_defer_frame(interp);
                        re_cache!();
                        continue;
                    } else {
                        let saved = std::mem::take(&mut self.defer_return).unwrap();
                        self.defer_stack = saved.parent_defers;
                        if saved.from_return {
                            match self.unwind_frame(Flow::Return(saved.return_value), interp) {
                                Unwind::Continue => {}
                                Unwind::Escaped(flow) => return Ok(flow),
                                Unwind::Error(e) => return Err(e),
                            }
                        } else {
                            if self.frames.is_empty() {
                                return Ok(Flow::Value(saved.return_value));
                            }
                            self.stack.push(saved.return_value);
                        }
                        re_cache!();
                        continue;
                    }
                }

                let defers: Vec<Value> = self.defer_stack.drain(..).collect();
                if !defers.is_empty() {
                    self.defer_return = Some(DeferReturn {
                        return_value: v,
                        remaining: defers,
                        parent_defers,
                        from_return: false,
                    });
                    self.push_defer_frame(interp);
                    re_cache!();
                    continue;
                }

                self.defer_stack = parent_defers;

                if self.frames.is_empty() {
                    return Ok(Flow::Value(v));
                }
                self.stack.push(v);
                re_cache!();
                continue;
            }

            let op = &code[ip];
            ip += 1;
            // NO frame.ip write-back here — ip lives in a register.

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
                        .or_else(|| {
                            interp
                                .funcs
                                .get(name)
                                .map(|fv| Value::Func(Box::new(fv.clone())))
                        })
                        .or_else(|| {
                            interp.natives.get(name).map(|entry| {
                                Value::Native(Box::new(NativeFunc {
                                    name: name.clone(),
                                    arity: entry.arity,
                                }))
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
                        return Err(self.error(format!("undefined variable `{name}`"), *span));
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
                Op::SlotAddInt { dst, src } => {
                    let base = self.frames.last().unwrap().stack_base;
                    let idx_dst = base + *dst as usize;
                    let idx_src = base + *src as usize;
                    match (&self.stack[idx_dst], &self.stack[idx_src]) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack[idx_dst] = Value::Int(*a + *b);
                        }
                        // Slow path: fall back to generic add semantics.
                        _ => {
                            let (a, b) = (self.stack[idx_dst].clone(), self.stack[idx_src].clone());
                            let span = Span::default();
                            let r = eval_binary(zz_frontend::ast::BinOp::Add, a, b, span)?;
                            self.stack[idx_dst] = r;
                        }
                    }
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
                    interp
                        .env
                        .borrow_mut()
                        .define(name, Value::Func(Box::new(fv)));
                    self.stack.push(Value::Unit);
                }
                Op::RegisterStruct { name, fields } => {
                    interp.structs.insert(name.clone(), fields.clone());
                    self.stack.push(Value::Unit);
                }
                Op::BinOp(op, span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    let v = eval_binary(*op, l, r, *span)?;
                    self.stack.push(v);
                }
                Op::UnOp(op, span) => {
                    let v = self.stack.pop().unwrap();
                    let v = eval_unary(*op, v, *span)?;
                    self.stack.push(v);
                }
                Op::Jump(target) => {
                    ip = *target;
                }
                Op::JumpIfFalse(target) => {
                    let v = self.stack.pop().unwrap();
                    if !v.is_truthy() {
                        ip = *target;
                    }
                }
                Op::JumpIfTrue(target) => {
                    let v = self.stack.pop().unwrap();
                    if v.is_truthy() {
                        ip = *target;
                    }
                }
                Op::JumpIfFalseBool(target, span) => {
                    let v = self.stack.pop().unwrap();
                    if !matches!(v, Value::Bool(_)) {
                        return Err(self.error("`if` condition must be a bool", *span));
                    }
                    if !v.is_truthy() {
                        ip = *target;
                    }
                }
                Op::Return => {
                    let v = self.stack.pop().unwrap();
                    let defers: Vec<Value> = self.defer_stack.drain(..).collect();
                    if defers.is_empty() {
                        match self.unwind_frame(Flow::Return(v), interp) {
                            Unwind::Continue => {
                                re_cache!();
                            }
                            Unwind::Escaped(flow) => return Ok(flow),
                            Unwind::Error(e) => return Err(e),
                        }
                    } else {
                        let parent_defers = self
                            .frames
                            .last()
                            .map(|f| f.defer_stack.clone())
                            .unwrap_or_default();
                        self.defer_return = Some(DeferReturn {
                            return_value: v,
                            remaining: defers,
                            parent_defers,
                            from_return: true,
                        });
                        self.push_defer_frame(interp);
                        re_cache!();
                    }
                }
                Op::ForSetup {
                    exit,
                    header,
                    span,
                    num_vars,
                } => {
                    let it = self.stack.pop().unwrap();
                    let (iterable, idx) = match it.clone() {
                        Value::Array(_) => (it, Value::Int(0)),
                        Value::Range(r) => (it, Value::Int(r.start)),
                        Value::Dict(_) => (it, Value::Int(0)),
                        other => {
                            return Err(self
                                .error(format!("cannot iterate a value of type `{other}`"), *span))
                        }
                    };
                    let stack_base = self.stack.len() - 1;
                    let total_slots = 2 + *num_vars as usize; // iterable + index + num_vars placeholders
                    self.loops.push(LoopInfo {
                        exit: *exit,
                        header: *header,
                        env: Rc::clone(&interp.env),
                        frame_idx: self.frames.len() - 1,
                        stack_base,
                        slots: total_slots,
                    });
                    self.stack.push(iterable);
                    self.stack.push(idx);
                    // Push num_vars placeholder items (Unit)
                    for _ in 0..*num_vars {
                        self.stack.push(Value::Unit);
                    }
                }
                Op::ForNext { vars, exit, in_env } => {
                    let num_vars = vars.len();
                    // Pop num_vars loop variables from previous iteration
                    self.stack.truncate(self.stack.len() - num_vars);
                    let idx = self.stack.pop().unwrap(); // pop index
                    let iterable_idx = self.stack.len() - 1;

                    // Inline dispatch — zero heap allocations for Range/Array hot paths.
                    // Extract data from stack first, then drop borrow, then mutate.
                    let iter_done: bool;
                    let next_idx: Value;
                    let push_val: Value;
                    let push_val2: Option<Value>; // for dict iteration with 2 vars
                    {
                        match (&self.stack[iterable_idx], &idx) {
                            (Value::Range(r), Value::Int(i)) => {
                                let i = *i;
                                let step = r.step;
                                let end = r.end;
                                let finished = if step > 0 { i >= end } else { i <= end };
                                iter_done = finished;
                                next_idx = Value::Int(i + step);
                                push_val = Value::Int(i);
                                push_val2 = None;
                            }
                            (Value::Array(arr), Value::Int(i)) => {
                                let i = *i;
                                if i >= arr.len() as i64 {
                                    iter_done = true;
                                    next_idx = Value::Unit;
                                    push_val = Value::Unit;
                                } else {
                                    iter_done = false;
                                    next_idx = Value::Int(i + 1);
                                    push_val = arr[i as usize].clone();
                                }
                                push_val2 = None;
                            }
                            (Value::Dict(pairs), Value::Int(i)) => {
                                let i = *i as usize;
                                if i >= pairs.len() {
                                    iter_done = true;
                                    next_idx = Value::Unit;
                                    push_val = Value::Unit;
                                    push_val2 = None;
                                } else {
                                    iter_done = false;
                                    next_idx = Value::Int(i as i64 + 1);
                                    push_val = pairs[i].0.clone();
                                    if num_vars == 2 {
                                        push_val2 = Some(pairs[i].1.clone());
                                    } else {
                                        push_val2 = None;
                                    }
                                }
                            }
                            _ => unreachable!("ForNext on non-iterable"),
                        }
                    } // immutable borrow of self.stack dropped here

                    if iter_done {
                        let li = self.loops.pop().unwrap();
                        self.stack.truncate(li.stack_base + 1);
                        interp.env = li.env;
                        ip = *exit;
                    } else {
                        self.stack.push(next_idx);
                        self.stack.push(push_val.clone());
                        if let Some(ref v) = push_val2 {
                            self.stack.push(v.clone());
                        }
                        if *in_env {
                            let li = self.loops.last().unwrap();
                            let loop_env = Rc::clone(&li.env);
                            interp.env = loop_env;
                            let scope = Env::with_parent(&interp.env);
                            if let Some(ref v2) = push_val2 {
                                scope.borrow_mut().define(&vars[0], push_val);
                                scope.borrow_mut().define(&vars[1], v2.clone());
                            } else {
                                scope.borrow_mut().define(&vars[0], push_val);
                            }
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
                        return Err(self.error("`while` condition must be a bool", *span));
                    }
                    if !c.is_truthy() {
                        let li = self.loops.pop().unwrap();
                        self.stack.truncate(li.stack_base + 1);
                        interp.env = li.env;
                        ip = *exit;
                    }
                }
                Op::Break(span) => {
                    let Some(li) = self.loops.pop() else {
                        return Err(self.error("`break` outside of a loop", *span));
                    };
                    if li.frame_idx != self.frames.len() - 1 {
                        return Err(self.error("`break` outside of a loop", *span));
                    }
                    self.stack.truncate(li.stack_base + 1);
                    interp.env = li.env;
                    ip = li.exit;
                }
                Op::Continue(span) => {
                    let Some(li) = self.loops.last() else {
                        return Err(self.error("`continue` outside of a loop", *span));
                    };
                    if li.frame_idx != self.frames.len() - 1 {
                        return Err(self.error("`continue` outside of a loop", *span));
                    }
                    self.stack.truncate(li.stack_base + 1 + li.slots);
                    interp.env = Rc::clone(&li.env);
                    ip = li.header;
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
                    self.stack.push(Value::Array(Box::new(items)));
                }
                Op::UnpackTuple(n) => {
                    let val = self.stack.pop().unwrap();
                    match val {
                        Value::Array(items) => {
                            if items.len() != *n as usize {
                                // This should be caught by the checker, but just in case.
                                return Err(self.error(
                                    format!(
                                        "expected tuple with {} elements, found {}",
                                        n,
                                        items.len()
                                    ),
                                    Span::default(),
                                ));
                            }
                            // Push elements in reverse so first is on top
                            for item in items.into_iter().rev() {
                                self.stack.push(item);
                            }
                        }
                        other => {
                            return Err(self.error(
                                format!("cannot unpack a value of type `{other}`"),
                                Span::default(),
                            ));
                        }
                    }
                }
                Op::ArrayPush(span) => {
                    let value = self.stack.pop().unwrap();
                    let mut arr = match self.stack.pop().unwrap() {
                        Value::Array(a) => a,
                        other => {
                            return Err(self.error(
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
                    self.stack.push(Value::Dict(Box::new(pairs)));
                }
                Op::IndexOp(span) => {
                    let iv = self.stack.pop().unwrap();
                    let ov = self.stack.pop().unwrap();
                    let v = get_index(&ov, &iv, *span)?;
                    self.stack.push(v);
                }
                Op::StoreIndexOp(span) => {
                    let mut ov = self.stack.pop().unwrap();
                    let iv = self.stack.pop().unwrap();
                    let value = self.stack.pop().unwrap();
                    set_index(&mut ov, &iv, value, *span)?;
                    self.stack.push(ov);
                }
                Op::SliceOp(span) => {
                    let e = self.stack.pop().unwrap();
                    let s = self.stack.pop().unwrap();
                    let ov = self.stack.pop().unwrap();
                    let bound = |v: Value| match v {
                        Value::Int(i) => Ok(Some(i)),
                        Value::Unit => Ok(None),
                        other => Err(self
                            .error(format!("slice bound must be `int`, found `{other}`"), *span)),
                    };
                    let v = slice_value(&ov, bound(s)?, bound(e)?, *span)?;
                    self.stack.push(v);
                }
                Op::MakeRange(span) => {
                    let e = self.stack.pop().unwrap();
                    let s = self.stack.pop().unwrap();
                    match (s, e) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Range(Box::new(RangeValue {
                                start: a,
                                end: b,
                                step: 1,
                            })))
                        }
                        _ => return Err(self.error("range bounds must be integers", *span)),
                    }
                }
                Op::MakeStruct {
                    name,
                    field_names,
                    span,
                } => {
                    let Some(registered) = interp.structs.get(name).cloned() else {
                        return Err(self.error(format!("unknown struct `{name}`"), *span));
                    };
                    let mut vals = Vec::with_capacity(field_names.len());
                    for _ in 0..field_names.len() {
                        vals.push(self.stack.pop().unwrap());
                    }
                    vals.reverse();
                    let mut out = Vec::with_capacity(registered.len());
                    for fname in &registered {
                        let Some(idx) = field_names.iter().position(|n| n == fname) else {
                            return Err(self.error(
                                format!("missing field `{fname}` in struct literal"),
                                *span,
                            ));
                        };
                        out.push((fname.clone(), vals[idx].clone()));
                    }
                    self.stack.push(Value::Object(Box::new(ObjectValue {
                        name: name.clone(),
                        fields: out,
                    })));
                }
                Op::GetField(name, span) => {
                    let ov = self.stack.pop().unwrap();
                    let v = object_field(&ov, name, *span)?;
                    self.stack.push(v);
                }
                Op::SetField(name, span) => {
                    let mut ov = self.stack.pop().unwrap();
                    let value = self.stack.pop().unwrap();
                    set_object_field(&mut ov, name, value, *span)?;
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
                    self.stack.push(Value::Func(Box::new(fv)));
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
                        ("ok", Some(v)) => self.stack.push(Value::Result(Box::new(Ok(v)))),
                        ("ok", None) => return Err(self.error("`.ok` requires an argument", *span)),
                        ("err", Some(v)) => self.stack.push(Value::Result(Box::new(Err(v)))),
                        ("err", None) => {
                            return Err(self.error("`.err` requires an argument", *span))
                        }
                        ("some", Some(v)) => self.stack.push(Value::Option(Some(Box::new(v)))),
                        ("some", None) => {
                            return Err(self.error("`.some` requires an argument", *span))
                        }
                        ("none", None) => self.stack.push(Value::Option(None)),
                        ("none", Some(_)) => {
                            return Err(self.error("`.none` takes no argument", *span))
                        }
                        (other, _) => {
                            return Err(self
                                .error(format!("unknown variant constructor `.{other}`"), *span))
                        }
                    }
                }
                Op::MatchArm {
                    pat,
                    next,
                    has_env,
                    restore,
                } => {
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
                        if *restore {
                            self.stack.push(sv);
                        }
                        ip = *next;
                    }
                }
                Op::MatchGuard { next, has_env } => {
                    let guard_val = self.stack.pop().unwrap();
                    match guard_val {
                        Value::Bool(true) => {}
                        _ => {
                            if *has_env {
                                // Exit the scope created by MatchArm
                                let parent = {
                                    let env = interp.env.borrow();
                                    env.parent_rc()
                                };
                                if let Some(env_ref) = parent {
                                    interp.env = env_ref;
                                }
                            }
                            ip = *next;
                        }
                    }
                }
                Op::MatchError(span) => {
                    return Err(self.error("non-exhaustive match: no arm matched", *span));
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
                        ip = *els;
                    }
                }
                Op::TryOp(span) => {
                    let v = self.stack.pop().unwrap();
                    match v {
                        Value::Option(Some(inner)) => self.stack.push(*inner),
                        Value::Option(None) => {
                            match self.unwind_frame(Flow::Return(Value::Option(None)), interp) {
                                Unwind::Continue => {
                                    re_cache!();
                                }
                                Unwind::Escaped(flow) => return Ok(flow),
                                Unwind::Error(e) => return Err(e),
                            }
                        }
                        Value::Result(r) => match &*r {
                            Ok(inner) => self.stack.push(inner.clone()),
                            Err(e) => {
                                match self.unwind_frame(
                                    Flow::Return(Value::Result(Box::new(Err(e.clone())))),
                                    interp,
                                ) {
                                    Unwind::Continue => {
                                        re_cache!();
                                    }
                                    Unwind::Escaped(flow) => return Ok(flow),
                                    Unwind::Error(err) => return Err(err),
                                }
                            }
                        },
                        other => {
                            return Err(self.error(
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
                            self.stack.push(Value::Bool(true));
                            self.stack.push(*inner);
                        }
                        Value::Option(None) => {
                            self.stack.push(Value::Bool(false));
                            self.stack.push(Value::Unit);
                        }
                        Value::Result(r) => match &*r {
                            Ok(inner) => {
                                self.stack.push(Value::Bool(true));
                                self.stack.push(inner.clone());
                            }
                            Err(_) => {
                                self.stack.push(Value::Bool(false));
                                self.stack.push(Value::Unit);
                            }
                        },
                        other => {
                            self.stack.push(Value::Bool(true));
                            self.stack.push(other);
                        }
                    }
                }
                Op::ElvisResult => {
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
                    // Sync ip back so the parent frame resumes at the right spot.
                    self.frames.last_mut().unwrap().ip = ip;
                    self.call_value(callee, args, span, interp)?;
                    re_cache!();
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
                            self.frames.last_mut().unwrap().ip = ip;
                            self.call_value(f, arg_vals, span, interp)?;
                            re_cache!();
                            continue;
                        }
                    }
                    let callee = interp.resolve_path_value(parts, pspan)?;
                    self.frames.last_mut().unwrap().ip = ip;
                    self.call_value(callee, args, span, interp)?;
                    re_cache!();
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
                    self.frames.last_mut().unwrap().ip = ip;
                    match object_field(&recv, name, span) {
                        Ok(f) => self.call_value(f, args, span, interp)?,
                        Err(_) => {
                            let f = interp.lookup_method(&recv, name, span)?;
                            let mut arg_vals = vec![recv];
                            arg_vals.extend(args);
                            self.call_value(f, arg_vals, span, interp)?;
                        }
                    }
                    re_cache!();
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
                    self.stack.push(Value::Str(out.into()));
                }
                Op::FormatValue(span) => {
                    let spec = self.stack.pop().unwrap();
                    let val = self.stack.pop().unwrap();
                    let spec_str = match spec {
                        Value::Str(s) => s,
                        _ => {
                            return Err(
                                self.error("format spec must be a string".to_string(), *span)
                            )
                        }
                    };
                    let formatted = crate::runtime::format::format_value_with_spec(&val, &spec_str);
                    self.stack.push(Value::Str(formatted.into()));
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
                Op::DeferRecord => {
                    let closure = self.stack.pop().unwrap();
                    self.defer_stack.push(closure);
                }
            }
        }
    }

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
                    return Err(self.error(
                        format!(
                            "expected {} arguments, found {}",
                            fv.params.len(),
                            args.len()
                        ),
                        span,
                    ));
                }
                let stack_base = self.stack.len();
                self.stack.extend(args);
                let prev_env = std::mem::replace(&mut interp.env, Rc::clone(&fv.env));
                let saved_defers = std::mem::take(&mut self.defer_stack);
                self.frames.push(Frame {
                    chunk: fv.chunk.unwrap(),
                    ip: 0,
                    prev_env,
                    stack_base,
                    defer_stack: saved_defers,
                    func_name: String::new(),
                    func_span: span,
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

    fn unwind_frame(&mut self, flow: Flow, interp: &mut Interp) -> Unwind {
        let v = match &flow {
            Flow::Return(v) => v.clone(),
            Flow::Break(_) | Flow::Continue(_) => Value::Unit,
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
            Flow::Break(span) => Unwind::Error(self.error("`break` outside of a loop", span)),
            Flow::Continue(span) => Unwind::Error(self.error("`continue` outside of a loop", span)),
            Flow::Value(_) => unreachable!(),
        }
    }

    /// Build a backtrace string from the current call stack.
    pub(crate) fn backtrace(&self) -> Vec<(String, Span)> {
        self.frames
            .iter()
            .map(|f| (f.func_name.clone(), f.func_span))
            .collect()
    }

    /// Create an EvalError with the current backtrace attached.
    fn error(&self, message: impl Into<String>, span: Span) -> EvalError {
        EvalError::new(message, span).with_backtrace(self.backtrace())
    }
}
