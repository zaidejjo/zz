use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use zz_frontend::ast::{Block, Expr};
use zz_frontend::span::Span;

use super::chunk::Chunk;
use super::op::Op;
use crate::env::Env;
use crate::eval::{EvalError, Interp};
use crate::runtime::ops::{
    eval_binary, eval_int_binary, eval_unary, get_index, object_field, set_index, set_object_field,
    slice_value,
};
use crate::runtime::Flow;
use crate::value::{FuncValue, NativeFunc, ObjectValue, RangeValue, Value};

/// Safepoint budget: iterations between timeslice clock reads. One counter
/// decrement + branch per iteration; the clock (`Instant::now`, ~20ns) runs
/// once per budget, so steady-state cost is ~0.02ns/iter — unmeasurable.
const SAFEPOINT_BUDGET: u32 = 1024;
/// Cooperative timeslice: a task that loops this long without blocking
/// yields its executor thread so siblings run. 1ms keeps interactive
/// (channel ping) latency low while requeue churn stays negligible.
const SAFEPOINT_QUANTUM_MS: u128 = 1;

/// One active call frame.
struct Frame {
    chunk: Arc<Chunk>,
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
    /// Frame depths (frames.len() after push) of pending `try` error-conversion
    /// calls. When such a frame returns, the value is wrapped in `Err` and the
    /// *caller* frame unwinds (early return) instead of continuing.
    try_convert_depths: Vec<usize>,
    /// Safepoint budget: iterations remaining before the next timeslice
    /// clock read. Reset to `SAFEPOINT_BUDGET` on expiry.
    slice_budget: u32,
    /// Start of the current cooperative timeslice. `None` until the first
    /// safepoint expiry (lazy: programs without loops never pay for a
    /// clock read, not even in `Vm::new`).
    slice_start: Option<std::time::Instant>,
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
            try_convert_depths: Vec::new(),
            slice_budget: SAFEPOINT_BUDGET,
            slice_start: None,
        }
    }

    /// Reset for shell-pool reuse (Phase 6 arena): clear all execution
    /// state but RETAIN buffer capacities, so the next task skips every
    /// `Vec` reallocation. The caller must have already dropped or moved
    /// out all `Value`s (stack/frames hold task-owned values — clearing
    /// here drops them; pooling only kicks in after completion, when the
    /// outcome was already extracted).
    pub fn reset(&mut self) {
        self.stack.clear();
        self.frames.clear();
        self.loops.clear();
        self.defer_stack.clear();
        self.defer_return = None;
        self.try_convert_depths.clear();
        self.slice_budget = SAFEPOINT_BUDGET;
        self.slice_start = None;
    }

    /// Push a value onto the VM stack. Used by `Interp::call_func` to set up
    /// compiled closure parameters before calling `run_chunk_with_base`,
    /// and by `task.spawn` to seat Unit args for worker closures (see
    /// `zz_stdlib::concurrency::spawn`: running a chunk with no args
    /// seated misaligns slot-indexed locals — every `LoadSlot` reads one
    /// slot off, yielding wrong values or out-of-bounds panics).
    pub fn push(&mut self, v: Value) {
        self.stack.push(v);
    }

    /// Replace the top of the stack. Used by the green-thread executor to
    /// deliver a blocking call's real result over the dummy value left by
    /// the yielded call op. Returns `false` when the stack is empty (a
    /// protocol violation — the executor treats it as a loud bug, never
    /// silent corruption).
    pub fn replace_top(&mut self, v: Value) -> bool {
        if let Some(top) = self.stack.last_mut() {
            *top = v;
            true
        } else {
            false
        }
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
    pub fn run_chunk(
        &mut self,
        chunk: &Arc<Chunk>,
        interp: &mut Interp,
    ) -> Result<Flow, EvalError> {
        self.run_chunk_with_base(chunk, interp, self.stack.len())
    }

    /// Like `run_chunk`, but allows the caller to specify `stack_base`
    /// explicitly. Used by `Interp::call_func` to run compiled closures
    /// where args are already on the stack at index 0..n, and by
    /// `task.spawn` to seat worker args at the stack bottom.
    pub fn run_chunk_with_base(
        &mut self,
        chunk: &Arc<Chunk>,
        interp: &mut Interp,
        stack_base: usize,
    ) -> Result<Flow, EvalError> {
        self.frames.push(Frame {
            chunk: Arc::clone(chunk),
            ip: 0,
            prev_env: Rc::clone(&interp.env),
            stack_base,
            defer_stack: Vec::new(),
            func_name: String::new(),
            func_span: Span::default(),
        });

        self.run_loop(interp)
    }

    /// Resume a suspended green-thread task: continue the existing frames
    /// without pushing a new one. Suspension preserved every frame's `ip`,
    /// so the loop picks up exactly where it yielded. (Pushing again here
    /// would re-execute the chunk from the start — the classic resume bug:
    /// duplicate spawns plus slot-index corruption from two frames sharing
    /// one stack base.)
    pub fn resume_chunk(&mut self, interp: &mut Interp) -> Result<Flow, EvalError> {
        self.run_loop(interp)
    }

    /// The interpreter loop shared by fresh and resumed execution.
    fn run_loop(&mut self, interp: &mut Interp) -> Result<Flow, EvalError> {
        // Cache chunk pointers locally to avoid re-fetching from frames on
        // every instruction. `ip` stays in a register; we only sync it back
        // to the Frame struct at frame-change points (Call/Return/defer).
        // Declared uninitialized: `re_cache!()` below fills all three from
        // the top frame (fresh frames start at ip 0; resumed tasks pick up
        // exactly where they yielded).
        let mut cached_code: *const Vec<Op>;
        let mut cached_constants: *const Vec<Value>;
        let mut ip: usize;

        // Re-cache from the current top frame (after any frame push/pop).
        macro_rules! re_cache {
            () => {{
                let f = self.frames.last().unwrap();
                let c = unsafe { &*Arc::as_ptr(&f.chunk) };
                cached_code = &c.code;
                cached_constants = &c.constants;
                ip = f.ip;
            }};
        }

        // Green-thread suspension: blocking natives (`chan.recv`,
        // `task.join`) request a yield instead of parking the thread when
        // running on the executor. The call op already completed (its dummy
        // result sits atop the stack for the executor to replace); the
        // frame ip was synced pre-call, so resumption continues right after
        // this op without any re-cache. Only the executor interprets
        // `Flow::Yield`.
        macro_rules! yield_check {
            () => {{
                if let Some(reason) = crate::value::take_yield() {
                    return Ok(Flow::Yield(reason));
                }
            }};
        }

        // Restore the register from the top frame. Fresh execution pushes
        // its frame with ip 0 (no-op here); resumed tasks continue exactly
        // where they yielded. Without this, every resume restarts the
        // chunk at 0 — re-running ForSetup, growing the stack, and
        // re-reading stale slots (latent until tasks first yielded
        // mid-chunk *and* resumed, which no test did before loop
        // safepoints made mid-chunk yields routine).
        re_cache!();

        loop {
            // SAFETY: cached_code/cached_constants point into the current
            // frame's Chunk which is kept alive by the Rc in self.frames.
            let code: &Vec<Op> = unsafe { &*cached_code };
            let constants: &Vec<Value> = unsafe { &*cached_constants };

            if ip >= code.len() {
                let sb = self.frames.last().unwrap().stack_base;
                let f = self.frames.last().unwrap();
                // Sync promoted top-level slots back into the environment so
                // later chunks (REPL statements, other modules) can read them.
                // Do this BEFORE popping the chunk result, since a `Keep`
                // declaration's value may live in a promoted slot.
                for (name, slot) in &f.chunk.toplevel_slots {
                    let idx = sb + *slot as usize;
                    if idx < self.stack.len() {
                        let val = self.stack[idx].clone();
                        interp.env.borrow_mut().define(name, val);
                    }
                }
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
                        // A `try`-conversion frame finishing its defers: wrap
                        // in `Err` and unwind the caller.
                        if self.try_convert_depths.last() == Some(&self.frames.len())
                            && !self.frames.is_empty()
                        {
                            self.try_convert_depths.pop();
                            match self.unwind_frame(
                                Flow::Return(Value::Result(Box::new(Err(saved.return_value)))),
                                interp,
                            ) {
                                Unwind::Continue => {}
                                Unwind::Escaped(flow) => return Ok(flow),
                                Unwind::Error(e) => return Err(e),
                            }
                            re_cache!();
                            continue;
                        }
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

                // Implicit chunk-end return of a `try`-conversion frame: the
                // frame was already popped above, so its depth is len()+1.
                if self.try_convert_depths.last() == Some(&(self.frames.len() + 1)) {
                    self.try_convert_depths.pop();
                    match self.unwind_frame(Flow::Return(Value::Result(Box::new(Err(v)))), interp) {
                        Unwind::Continue => {}
                        Unwind::Escaped(flow) => return Ok(flow),
                        Unwind::Error(e) => return Err(e),
                    }
                    re_cache!();
                    continue;
                }

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
                Op::SlotInc { slot } => {
                    let base = self.frames.last().unwrap().stack_base;
                    let idx = base + *slot as usize;
                    match &self.stack[idx] {
                        Value::Int(a) => self.stack[idx] = Value::Int(*a + 1),
                        _ => {
                            let v = self.stack[idx].clone();
                            let span = Span::default();
                            let r =
                                eval_binary(zz_frontend::ast::BinOp::Add, v, Value::Int(1), span)?;
                            self.stack[idx] = r;
                        }
                    }
                }
                Op::SlotAddIntImm { dst, imm } => {
                    let base = self.frames.last().unwrap().stack_base;
                    let idx = base + *dst as usize;
                    match &self.stack[idx] {
                        Value::Int(a) => self.stack[idx] = Value::Int(*a + *imm),
                        _ => {
                            let v = self.stack[idx].clone();
                            let span = Span::default();
                            let r = eval_binary(
                                zz_frontend::ast::BinOp::Add,
                                v,
                                Value::Int(*imm),
                                span,
                            )?;
                            self.stack[idx] = r;
                        }
                    }
                }
                Op::SlotLessIntSlot { a, b } => {
                    let base = self.frames.last().unwrap().stack_base;
                    let idx_a = base + *a as usize;
                    let idx_b = base + *b as usize;
                    match (&self.stack[idx_a], &self.stack[idx_b]) {
                        (Value::Int(x), Value::Int(y)) => {
                            self.stack.push(Value::Bool(x < y));
                        }
                        _ => {
                            let (l, r) = (self.stack[idx_a].clone(), self.stack[idx_b].clone());
                            let span = Span::default();
                            let v = eval_binary(zz_frontend::ast::BinOp::Lt, l, r, span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::SlotLessIntImm { a, imm } => {
                    let base = self.frames.last().unwrap().stack_base;
                    let idx = base + *a as usize;
                    match &self.stack[idx] {
                        Value::Int(x) => self.stack.push(Value::Bool(x < imm)),
                        _ => {
                            let l = self.stack[idx].clone();
                            let span = Span::default();
                            let v = eval_binary(
                                zz_frontend::ast::BinOp::Lt,
                                l,
                                Value::Int(*imm),
                                span,
                            )?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::SlotBinaryInt { dst, lhs, rhs, op } => {
                    let base = self.frames.last().unwrap().stack_base;
                    let idx_d = base + *dst as usize;
                    let idx_l = base + *lhs as usize;
                    let idx_r = base + *rhs as usize;
                    match (&self.stack[idx_l], &self.stack[idx_r]) {
                        (Value::Int(a), Value::Int(b)) => {
                            let v = eval_int_binary(*op, *a, *b, Span::default())?;
                            self.stack[idx_d] = v;
                        }
                        _ => {
                            let (l, r) = (self.stack[idx_l].clone(), self.stack[idx_r].clone());
                            let span = Span::default();
                            let v = eval_binary(*op, l, r, span)?;
                            self.stack[idx_d] = v;
                        }
                    }
                }
                Op::SlotBinaryIntImm { dst, lhs, imm, op } => {
                    let base = self.frames.last().unwrap().stack_base;
                    let idx_d = base + *dst as usize;
                    let idx_l = base + *lhs as usize;
                    match &self.stack[idx_l] {
                        Value::Int(a) => {
                            let v = eval_int_binary(*op, *a, *imm, Span::default())?;
                            self.stack[idx_d] = v;
                        }
                        _ => {
                            let l = self.stack[idx_l].clone();
                            let span = Span::default();
                            let v = eval_binary(*op, l, Value::Int(*imm), span)?;
                            self.stack[idx_d] = v;
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
                        chunk: Some(Arc::clone(fchunk)),
                    };
                    interp.funcs.insert(name.clone(), fv.clone());
                    interp.funcs_version = interp.funcs_version.wrapping_add(1);
                    interp
                        .env
                        .borrow_mut()
                        .define(name, Value::Func(Box::new(fv)));
                    self.stack.push(Value::Unit);
                }
                Op::RegisterStruct { name, fields } => {
                    // Copy-on-write (see tree-walker `Stmt::Struct`).
                    Arc::make_mut(&mut interp.structs).insert(name.clone(), fields.clone());
                    self.stack.push(Value::Unit);
                }
                Op::BinOp(op, span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    let v = eval_binary(*op, l, r, *span)?;
                    self.stack.push(v);
                }
                Op::IntAdd(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            #[cfg(not(debug_assertions))]
                            {
                                self.stack.push(Value::Int(a.wrapping_add(*b)));
                            }
                            #[cfg(debug_assertions)]
                            {
                                let v = a.checked_add(*b).ok_or_else(|| {
                                    EvalError::new("integer overflow in addition", *span)
                                })?;
                                self.stack.push(Value::Int(v));
                            }
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Add, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntSub(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            #[cfg(not(debug_assertions))]
                            {
                                self.stack.push(Value::Int(a.wrapping_sub(*b)));
                            }
                            #[cfg(debug_assertions)]
                            {
                                let v = a.checked_sub(*b).ok_or_else(|| {
                                    EvalError::new("integer overflow in subtraction", *span)
                                })?;
                                self.stack.push(Value::Int(v));
                            }
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Sub, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntMul(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            #[cfg(not(debug_assertions))]
                            {
                                self.stack.push(Value::Int(a.wrapping_mul(*b)));
                            }
                            #[cfg(debug_assertions)]
                            {
                                let v = a.checked_mul(*b).ok_or_else(|| {
                                    EvalError::new("integer overflow in multiplication", *span)
                                })?;
                                self.stack.push(Value::Int(v));
                            }
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Mul, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntDiv(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(_), Value::Int(0)) => {
                            return Err(EvalError::new("division by zero", *span));
                        }
                        (Value::Int(a), Value::Int(b)) => {
                            #[cfg(not(debug_assertions))]
                            {
                                self.stack.push(Value::Int(a.wrapping_div(*b)));
                            }
                            #[cfg(debug_assertions)]
                            {
                                let v = a.checked_div(*b).ok_or_else(|| {
                                    EvalError::new("integer overflow in division", *span)
                                })?;
                                self.stack.push(Value::Int(v));
                            }
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Div, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntRem(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(_), Value::Int(0)) => {
                            return Err(EvalError::new("modulo by zero", *span));
                        }
                        (Value::Int(a), Value::Int(b)) => {
                            #[cfg(not(debug_assertions))]
                            {
                                self.stack.push(Value::Int(a.wrapping_rem(*b)));
                            }
                            #[cfg(debug_assertions)]
                            {
                                let v = a.checked_rem(*b).ok_or_else(|| {
                                    EvalError::new("integer overflow in modulo", *span)
                                })?;
                                self.stack.push(Value::Int(v));
                            }
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Rem, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntNeg(span) => {
                    let v = self.stack.pop().unwrap();
                    match &v {
                        Value::Int(a) => {
                            #[cfg(not(debug_assertions))]
                            {
                                self.stack.push(Value::Int(a.wrapping_neg()));
                            }
                            #[cfg(debug_assertions)]
                            {
                                let r = a.checked_neg().ok_or_else(|| {
                                    EvalError::new("integer overflow in negation", *span)
                                })?;
                                self.stack.push(Value::Int(r));
                            }
                        }
                        _ => {
                            let v = eval_unary(zz_frontend::ast::UnOp::Neg, v, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::UnOp(op, span) => {
                    let v = self.stack.pop().unwrap();
                    let v = eval_unary(*op, v, *span)?;
                    self.stack.push(v);
                }
                Op::Jump(target) => {
                    ip = *target;
                }
                Op::Safepoint => {
                    // Cooperative safepoint (see `Op::Safepoint` docs): one
                    // counter decrement per iteration, clock read once per
                    // budget. `ip` already advanced past this op, so a
                    // yield here resumes after it — but the frame's saved
                    // ip must be synced first (the register is only
                    // written back at frame-change points otherwise).
                    if self.slice_budget == 0 {
                        self.slice_budget = SAFEPOINT_BUDGET;
                        let now = std::time::Instant::now();
                        let expired = self.slice_start.is_none_or(|t| {
                            now.duration_since(t).as_millis() >= SAFEPOINT_QUANTUM_MS
                        });
                        if expired {
                            self.slice_start = Some(now);
                            if crate::value::on_executor() {
                                self.frames.last_mut().unwrap().ip = ip;
                                return Ok(Flow::Yield(crate::value::YieldReason::Timeslice));
                            }
                        }
                    } else {
                        self.slice_budget -= 1;
                    }
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
                        // A `try`-conversion call returns here (no defers of its
                        // own): wrap in `Err` and unwind the caller instead of
                        // continuing. With defers, the flag stays set and the
                        // defer-completion path wraps after they run.
                        if self.pop_try_convert_flag() {
                            match self
                                .unwind_frame(Flow::Return(Value::Result(Box::new(Err(v)))), interp)
                            {
                                Unwind::Continue => {
                                    re_cache!();
                                }
                                Unwind::Escaped(flow) => return Ok(flow),
                                Unwind::Error(e) => return Err(e),
                            }
                            continue;
                        }
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
                Op::ForNext {
                    vars, exit, in_env, ..
                } => {
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
                Op::GetFieldIdx(idx, span) => {
                    let ov = self.stack.pop().unwrap();
                    match ov {
                        Value::Object(o) => {
                            if (*idx as usize) < o.fields.len() {
                                self.stack.push(o.fields[*idx as usize].1.clone());
                            } else {
                                return Err(EvalError::new(
                                    format!(
                                        "struct `{}` field index {} out of bounds",
                                        o.name, idx
                                    ),
                                    *span,
                                ));
                            }
                        }
                        _ => {
                            return Err(EvalError::new(
                                "expected struct for indexed field access".to_string(),
                                *span,
                            ));
                        }
                    }
                }
                Op::SetField(name, span) => {
                    let mut ov = self.stack.pop().unwrap();
                    let value = self.stack.pop().unwrap();
                    set_object_field(&mut ov, name, value, *span)?;
                    self.stack.push(ov);
                }
                Op::SetFieldIdx(idx, span) => {
                    let mut ov = self.stack.pop().unwrap();
                    let value = self.stack.pop().unwrap();
                    match &mut ov {
                        Value::Object(o) => {
                            if (*idx as usize) < o.fields.len() {
                                o.fields[*idx as usize].1 = value;
                            } else {
                                return Err(EvalError::new(
                                    format!(
                                        "struct `{}` field index {} out of bounds",
                                        o.name, idx
                                    ),
                                    *span,
                                ));
                            }
                        }
                        _ => {
                            return Err(EvalError::new(
                                "expected struct for indexed field access".to_string(),
                                *span,
                            ));
                        }
                    }
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
                        chunk: Some(Arc::clone(chunk)),
                    };
                    self.stack.push(Value::Func(Box::new(fv)));
                }
                Op::SpawnClosure {
                    params,
                    chunk,
                    span,
                } => {
                    // Fused spawn (see `SpawnHook`): the chunk + params go
                    // straight to the task constructor — no FuncValue box,
                    // no args Vec, no native lookup. Creation env is the
                    // current env, exactly as MakeClosure would capture.
                    let hook = crate::eval::SPAWN_HOOK.get().copied().ok_or_else(|| {
                        self.error("`task.spawn` used without stdlib task support", *span)
                    })?;
                    let v = hook(interp, chunk, params, *span)?;
                    self.stack.push(v);
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
                                // V1 conversion: single `convert_to_*` candidate
                                // for the error source type is called; the flag
                                // makes its return unwind as `Err` (see Return).
                                let conv = self.find_convert_name(e, interp);
                                match conv {
                                    None => match self.unwind_frame(
                                        Flow::Return(Value::Result(Box::new(Err(e.clone())))),
                                        interp,
                                    ) {
                                        Unwind::Continue => {
                                            re_cache!();
                                        }
                                        Unwind::Escaped(flow) => return Ok(flow),
                                        Unwind::Error(err) => return Err(err),
                                    },
                                    Some(fname) => {
                                        let Some(fv) = interp.funcs.get(&fname).cloned() else {
                                            match self.unwind_frame(
                                                Flow::Return(Value::Result(Box::new(Err(
                                                    e.clone()
                                                )))),
                                                interp,
                                            ) {
                                                Unwind::Continue => {
                                                    re_cache!();
                                                }
                                                Unwind::Escaped(flow) => return Ok(flow),
                                                Unwind::Error(err) => return Err(err),
                                            }
                                            continue;
                                        };
                                        let span_c = *span;
                                        let err_c = e.clone();
                                        if fv.chunk.is_some() {
                                            self.frames.last_mut().unwrap().ip = ip;
                                            let callee = Value::Func(Box::new(fv));
                                            self.call_value(callee, vec![err_c], span_c, interp)?;
                                            self.try_convert_depths.push(self.frames.len());
                                            re_cache!();
                                        } else {
                                            let callee = Value::Func(Box::new(fv));
                                            let converted =
                                                interp.call(callee, vec![err_c], span_c)?;
                                            match self.unwind_frame(
                                                Flow::Return(Value::Result(Box::new(Err(
                                                    converted,
                                                )))),
                                                interp,
                                            ) {
                                                Unwind::Continue => {
                                                    re_cache!();
                                                }
                                                Unwind::Escaped(flow) => return Ok(flow),
                                                Unwind::Error(err) => return Err(err),
                                            }
                                        }
                                    }
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
                    yield_check!();
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
                            // sqlz fast path (CallPath form): Db handle +
                            // query/exec/close dispatches straight to the
                            // canonical sqlz.* native (db.* alias fallback).
                            // lookup_method would also work via the Db
                            // method_namespace, but this avoids the
                            // object_field detour entirely.
                            if matches!(recv, Value::Db(_))
                                && matches!(method.as_str(), "query" | "exec" | "close")
                            {
                                let entry = interp
                                    .natives
                                    .get(&format!("sqlz.{method}"))
                                    .copied()
                                    .or_else(|| {
                                        interp.natives.get(&format!("std.sqlz.{method}")).copied()
                                    })
                                    .or_else(|| {
                                        interp.natives.get(&format!("db.{method}")).copied()
                                    })
                                    .or_else(|| {
                                        interp.natives.get(&format!("std.db.{method}")).copied()
                                    });
                                match entry {
                                    Some(e) => {
                                        let mut arg_vals = vec![recv];
                                        arg_vals.extend(args);
                                        self.frames.last_mut().unwrap().ip = ip;
                                        let result = (e.f)(interp, &mut arg_vals, span)?;
                                        self.stack.push(result);
                                        re_cache!();
                                        yield_check!();
                                        continue;
                                    }
                                    None => {
                                        return Err(self
                                            .error(format!("undefined method `{method}`"), span));
                                    }
                                }
                            }
                            let f = interp.lookup_method(&recv, method, pspan)?;
                            let mut arg_vals = vec![recv];
                            arg_vals.extend(args);
                            self.frames.last_mut().unwrap().ip = ip;
                            self.call_value(f, arg_vals, span, interp)?;
                            re_cache!();
                            yield_check!();
                            continue;
                        }
                    }
                    let callee = interp.resolve_path_value(parts, pspan)?;
                    self.frames.last_mut().unwrap().ip = ip;
                    self.call_value(callee, args, span, interp)?;
                    re_cache!();
                    yield_check!();
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
                    // sqlz fast path: `mydb.query/exec` on a Db handle
                    // bypasses object_field (Db has no struct fields) and
                    // dispatches straight to the canonical sqlz.* native
                    // (db.* alias fallback).
                    if matches!(recv, Value::Db(_))
                        && matches!(name.as_str(), "query" | "exec" | "close")
                    {
                        let native_name = format!("sqlz.{name}");
                        let entry = interp
                            .natives
                            .get(&native_name)
                            .copied()
                            .or_else(|| interp.natives.get(&format!("std.sqlz.{name}")).copied())
                            .or_else(|| interp.natives.get(&format!("db.{name}")).copied())
                            .or_else(|| interp.natives.get(&format!("std.db.{name}")).copied());
                        match entry {
                            Some(e) => {
                                let mut arg_vals = vec![recv];
                                arg_vals.extend(args);
                                let result = (e.f)(interp, &mut arg_vals, span)?;
                                self.stack.push(result);
                                re_cache!();
                                yield_check!();
                                continue;
                            }
                            None => {
                                return Err(self.error(format!("undefined method `{name}`"), span));
                            }
                        }
                    }
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
                    yield_check!();
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
                Op::DbQuery { nparams, span } => {
                    // Stack: [template, p1..pn] (template pushed first by
                    // the compiler). Re-push template then params in order
                    // so CallNative sees [template, p1..pn].
                    let n = *nparams as usize;
                    if self.stack.len() < n + 1 {
                        return Err(
                            self.error("db query stack underflow in DbQuery".to_string(), *span)
                        );
                    }
                    let mut params = Vec::with_capacity(n);
                    for _ in 0..n {
                        params.push(self.stack.pop().unwrap());
                    }
                    params.reverse();
                    let template = self.stack.pop().unwrap();
                    self.stack.push(template);
                    for p in params {
                        self.stack.push(p);
                    }
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
                Op::CallNative { name, argc, span } => {
                    let argc = *argc;
                    let span = *span;
                    let mut args = Vec::with_capacity(argc as usize);
                    for _ in 0..argc {
                        args.push(self.stack.pop().unwrap());
                    }
                    args.reverse();
                    // sqlz.query/sqlz.exec (+ db.* alias) carry a variable
                    // number of bound params; resolve the entry without an
                    // arity gate (Interp::call skips it for sqlz.* too).
                    let entry =
                        interp.natives.get(name).copied().ok_or_else(|| {
                            EvalError::new(format!("unknown native `{name}`"), span)
                        })?;
                    self.frames.last_mut().unwrap().ip = ip;
                    let result = (entry.f)(interp, &mut args, span)?;
                    self.stack.push(result);
                    re_cache!();
                    yield_check!();
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

    /// Receiver key for `try` conversion lookup (mirrors tree-walker).
    fn convert_recv_key(recv: &Value) -> Option<String> {
        match recv {
            Value::Str(_) => Some("str".to_string()),
            Value::Array(_) => Some("vec".to_string()),
            Value::Option(_) => Some("option".to_string()),
            Value::Result(_) => Some("result".to_string()),
            Value::Int(_) => Some("int".to_string()),
            Value::Float(_) => Some("float".to_string()),
            Value::Bool(_) => Some("bool".to_string()),
            Value::Object(o) => Some(o.name.clone()),
            _ => None,
        }
    }

    /// Single `Type.convert_to_*` candidate for an error value, if any.
    /// Returns the function name; the caller resolves it to a `Value`.
    fn find_convert_name(&self, err: &Value, interp: &Interp) -> Option<String> {
        let key = Self::convert_recv_key(err)?;
        let prefix = format!("{key}.convert_to_");
        let mut hits: Vec<String> = interp
            .funcs
            .keys()
            .filter(|n| n.starts_with(&prefix))
            .cloned()
            .collect();
        if hits.is_empty() {
            let suffix = format!(".{prefix}");
            hits = interp
                .funcs
                .keys()
                .filter(|n| n.contains(&suffix))
                .cloned()
                .collect();
        }
        if hits.len() == 1 {
            hits.into_iter().next()
        } else {
            None
        }
    }

    /// True when the current frame return belongs to a pending `try` conversion.
    fn pop_try_convert_flag(&mut self) -> bool {
        if self.try_convert_depths.last() == Some(&self.frames.len()) {
            self.try_convert_depths.pop();
            true
        } else {
            false
        }
    }

    fn unwind_frame(&mut self, flow: Flow, interp: &mut Interp) -> Unwind {
        let v = match &flow {
            Flow::Return(v) => v.clone(),
            Flow::Break(_) | Flow::Continue(_) => Value::Unit,
            Flow::Value(_) => unreachable!("unwind_frame on a plain value"),
            Flow::Yield(_) => return Unwind::Error(crate::runtime::EvalError::yield_escape()),
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
            Flow::Yield(_) => Unwind::Error(crate::runtime::EvalError::yield_escape()),
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
