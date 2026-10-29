use std::sync::Arc;

use zz_frontend::ast::{BinOp, Block, Expr};
use zz_frontend::span::Span;

use super::chunk::Chunk;
use super::op::{Op, TakeHome};
use crate::env::{Bound, Env, EnvLink};
use crate::eval::{EvalError, Interp};
use crate::runtime::ops::{
    eval_binary, eval_int_binary, eval_unary, fill_default_headers, get_index, normalize_index,
    object_field, object_field_opt, set_index, set_object_field, slice_value,
};
use crate::runtime::Flow;
use crate::value::{FuncValue, NativeFunc, RangeValue, Value};

/// Safepoint budget: iterations between timeslice clock reads. One counter
/// decrement + branch per iteration; the clock (`Instant::now`, ~20ns) runs
/// once per budget, so steady-state cost is ~0.02ns/iter — unmeasurable.
const SAFEPOINT_BUDGET: u32 = 1024;
/// Cooperative timeslice: a task that loops this long without blocking
/// yields its executor thread so siblings run. 1ms keeps interactive
/// (channel ping) latency low while requeue churn stays negligible.
const SAFEPOINT_QUANTUM_MS: u128 = 1;

/// Overwrite a stack slot in place without running `drop_in_place` on the
/// value it replaces.
///
/// The int-specialised slot ops match the slot as `Value::Int` in the same
/// arm (and `ForNext`'s fast paths only ever replace an `Int` index or a
/// loop-var slot this op last filled — `Unit` on the first iteration, `Int`
/// afterwards), so the old value owns no heap payload and dropping it is a
/// no-op. The drop glue for the 16-variant `Value` enum is a call that LLVM
/// cannot see through; measured at ~3ns per write on a 16-byte `Value`,
/// roughly a fifth of a `SlotAddInt` iteration.
///
/// The caller still indexes the stack normally, so a bad slot index panics
/// exactly as before — only the drop is elided.
///
/// # Safety
///
/// The value currently in `slot` must be a scalar (`Int`, `Float`, `Bool` or
/// `Unit`): nothing that needs dropping.
#[inline(always)]
unsafe fn overwrite_scalar(slot: &mut Value, v: Value) {
    debug_assert!(
        matches!(
            slot,
            Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Unit
        ),
        "overwrite_scalar replaced a heap-owning value"
    );
    unsafe { std::ptr::write(slot, v) };
}

/// Inlined `i64` comparison for the fused `SlotCmpInt*` ops.
///
/// `None` for operators the compiler never emits in a comparison slot (the
/// handlers fall back to `eval_int_binary`, which also keeps the generic
/// result type — `Int` for arithmetic).
#[inline]
fn int_cmp(op: BinOp, a: i64, b: i64) -> Option<bool> {
    Some(match op {
        BinOp::Eq => a == b,
        BinOp::Ne => a != b,
        BinOp::Lt => a < b,
        BinOp::Gt => a > b,
        BinOp::Le => a <= b,
        BinOp::Ge => a >= b,
        _ => return None,
    })
}

/// Write `v` into a user-assignable slot, skipping the drop glue when the
/// slot currently holds a scalar.
///
/// Unlike the int-specialised slot ops (`x = x + y`), this slot can hold
/// anything: a loop variable is assignable (`for i in 0..3 { i = 5 }`), and
/// dynamic code can put a heap value there. The scalar match keeps the
/// elision safe and costs one predictable branch.
#[inline(always)]
fn write_slot_checked(slot: &mut Value, v: Value) {
    if matches!(
        slot,
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Unit
    ) {
        // SAFETY: matched as a scalar just above.
        unsafe { overwrite_scalar(slot, v) };
    } else {
        *slot = v;
    }
}

// Per-thread scratch args buffer for native calls (see the `CallNative`
// arm): one reusable allocation instead of one per call.
thread_local! {
    static SCRATCH_ARGS: std::cell::RefCell<Option<Vec<Value>>> =
        const { std::cell::RefCell::new(None) };
}

/// Take the scratch args buffer (cleared), or a fresh `Vec` when
/// reentered (a native calling back into the VM) or leaked by a past
/// panic unwind.
fn take_scratch_args() -> Vec<Value> {
    SCRATCH_ARGS.with(|cell| {
        cell.borrow_mut()
            .take()
            .map(|mut v| {
                v.clear();
                v
            })
            .unwrap_or_else(Vec::new)
    })
}

/// Return a used args buffer to the scratch slot (keeps capacity).
/// Dropped instead when reentered (the outer call still owns the slot).
fn return_scratch_args(mut args: Vec<Value>) {
    args.clear();
    SCRATCH_ARGS.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(args);
        }
    });
}

/// One active call frame.
struct Frame {
    chunk: Arc<Chunk>,
    ip: usize,
    /// Environment to restore when this frame returns.
    prev_env: EnvLink,
    /// Stack index where this frame's evaluation begins.
    stack_base: usize,
    /// Deferred closures accumulated in this frame. Saved/restored across
    /// nested calls so each frame only drains its own defers.
    defer_stack: Vec<Value>,
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
    env: EnvLink,
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
    /// Attached debugger session, shared so breakpoints (and step
    /// rules) update live while parked. `None` (the default) disables
    /// the per-op check to a single `Option` discriminant test.
    pub debug: Option<std::sync::Arc<std::sync::Mutex<crate::debug::DebugSession>>>,
    /// Memoized `CallPath` resolution, indexed by `(chunk, ip)` — see
    /// [`Vm::direct_call_hit`].
    call_cache: Vec<Option<CachedDirect>>,
}

/// One memoized `CallPath` resolution.
///
/// `gen` pins the environment + function-table state the entry was resolved
/// against; any define/assign/freeze/insert invalidates every entry at once
/// (shadowing is dynamic, see [`crate::env::ENV_GEN`]).
struct CachedDirect {
    chunk_id: u64,
    ip: usize,
    argc: u16,
    gen: u64,
    callee: Arc<Chunk>,
    env: crate::env::EnvLink,
}

/// Power-of-two size of [`Vm::call_cache`] (direct-mapped: a collision just
/// costs one extra resolution).
const CALL_CACHE_LEN: usize = 256;

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
            debug: None,
            call_cache: (0..CALL_CACHE_LEN).map(|_| None).collect(),
        }
    }

    /// Attach a debugger session: line breakpoints suspend the run loop
    /// via [`YieldReason::DebugBreak`](crate::value::YieldReason).
    /// Detaching (`None`) restores full speed.
    pub fn attach_debug(&mut self, session: crate::debug::DebugSession) {
        self.debug = Some(std::sync::Arc::new(std::sync::Mutex::new(session)));
    }

    /// Shared handle to the attached session (for live breakpoint
    /// updates while parked or running).
    pub fn debug_session(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::debug::DebugSession>>> {
        self.debug.clone()
    }

    /// Stack snapshot for the debugger: innermost-first
    /// (function name, source label, op span).
    pub fn debug_frames(&self) -> Vec<(String, Option<String>, zz_frontend::span::Span)> {
        self.frames
            .iter()
            .rev()
            .map(|f| {
                let span = f.chunk.spans.get(f.ip).copied().unwrap_or_default();
                (
                    String::new(),
                    f.chunk.source.as_deref().map(|s| s.to_string()),
                    span,
                )
            })
            .collect()
    }

    /// Variables visible in frame `idx` (innermost-first index into
    /// [`debug_frames`](Self::debug_frames)): slot names plus
    /// environment names, sorted, shadowing resolved slot-first.
    pub fn debug_frame_vars(
        &self,
        interp: &crate::eval::Interp,
        idx: usize,
    ) -> Vec<(String, crate::value::Value)> {
        let frames: Vec<&Frame> = self.frames.iter().rev().collect();
        let Some(frame) = frames.get(idx) else {
            return Vec::new();
        };
        let mut out: Vec<(String, crate::value::Value)> = Vec::new();
        for (name, slot) in &frame.chunk.toplevel_slots {
            if let Some(v) = self.stack.get(frame.stack_base + *slot as usize) {
                out.push((name.clone(), v.clone()));
            }
        }
        // The visible environment: the caller's saved env for inner
        // frames, the live env for the top frame.
        let env = if idx + 1 < frames.len() {
            &frames[idx + 1].prev_env
        } else {
            &interp.env
        };
        for name in env.local_names() {
            if out.iter().any(|(n, _)| n == &name) {
                continue;
            }
            if let Some(v) = env.get(&name) {
                out.push((name, v));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Resolve a variable path for the variables view: the named root
    /// in frame `idx`, then [`DebugSeg`](crate::debug::DebugSeg) steps.
    /// Returns `None` for unknown roots, out-of-range indexes, and
    /// missing fields — the adapter reports an empty scope instead of
    /// guessing.
    pub fn debug_resolve(
        &self,
        interp: &crate::eval::Interp,
        idx: usize,
        root: &str,
        path: &[crate::debug::DebugSeg],
    ) -> Option<crate::value::Value> {
        let mut value = self
            .debug_frame_vars(interp, idx)
            .into_iter()
            .find(|(name, _)| name == root)
            .map(|(_, value)| value)?;
        for seg in path {
            value = match seg {
                crate::debug::DebugSeg::Field(name) => {
                    crate::debug::debug_child_named(&value, name)?
                }
                crate::debug::DebugSeg::Index(i) => match &value {
                    crate::value::Value::Array(a) | crate::value::Value::Tuple(a) => {
                        a.get(*i)?.clone()
                    }
                    _ => return None,
                },
            };
        }
        Some(value)
    }

    /// Look up a variable where execution currently stands: the top
    /// frame's slots first (top-level lets live there until frame exit
    /// syncs them into the environment), then the interpreter env.
    /// Deeper frames and closure captures follow in later work.
    pub fn debug_lookup(
        &self,
        interp: &crate::eval::Interp,
        name: &str,
    ) -> Option<crate::value::Value> {
        if let Some(frame) = self.frames.last() {
            if let Some((_, slot)) = frame.chunk.toplevel_slots.iter().find(|(n, _)| n == name) {
                if let Some(v) = self.stack.get(frame.stack_base + *slot as usize) {
                    return Some(v.clone());
                }
            }
        }
        interp.env.get(name)
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
        // Debugger sessions never survive pooling: a leftover breakpoint
        // set would suspend unrelated reused tasks.
        self.debug = None;
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
            if let Some(chunk) = fv.chunk.clone() {
                let stack_base = self.stack.len();
                let prev_env = std::mem::replace(&mut interp.env, fv.env.clone());
                let saved_defers = std::mem::take(&mut self.defer_stack);
                self.frames.push(Frame {
                    chunk,
                    ip: 0,
                    prev_env,
                    stack_base,
                    defer_stack: saved_defers,
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
            prev_env: interp.env.clone(),
            stack_base,
            defer_stack: Vec::new(),
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

    /// One cooperative-safepoint tick: decrement the call-frame slice budget
    /// and, once per budget, read the clock to see whether the current
    /// timeslice expired.
    ///
    /// Returns `true` only on executor threads, where a yield is actionable
    /// (`zz run` has no executor and burns no clock here): the caller must
    /// then rewind `ip` to the op it is executing and return
    /// [`Flow::Yield`] so that resuming re-executes that op from the top.
    fn safepoint_expired(&mut self) -> bool {
        if self.slice_budget == 0 {
            self.slice_budget = SAFEPOINT_BUDGET;
            let now = std::time::Instant::now();
            let expired = self
                .slice_start
                .is_none_or(|t| now.duration_since(t).as_millis() >= SAFEPOINT_QUANTUM_MS);
            if expired {
                self.slice_start = Some(now);
                return crate::value::on_executor();
            }
            false
        } else {
            self.slice_budget -= 1;
            false
        }
    }

    /// The interpreter loop shared by fresh and resumed execution.
    ///
    /// Split on a const-generic `STATS` flag so the `ZZ_VM_STATS` counters
    /// cost exactly nothing when off: the counter branch is compiled out of
    /// the `STATS = false` instantiation instead of being tested per op.
    /// Exactly one instantiation runs per process.
    fn run_loop(&mut self, interp: &mut Interp) -> Result<Flow, EvalError> {
        if super::stats::enabled() {
            self.run_loop_impl::<true>(interp)
        } else {
            self.run_loop_impl::<false>(interp)
        }
    }

    fn run_loop_impl<const STATS: bool>(&mut self, interp: &mut Interp) -> Result<Flow, EvalError> {
        // Cache chunk pointers locally to avoid re-fetching from frames on
        // every instruction. `ip` stays in a register; we only sync it back
        // to the Frame struct at frame-change points (Call/Return/defer).
        // Declared uninitialized: `re_cache!()` below fills all three from
        // the top frame (fresh frames start at ip 0; resumed tasks pick up
        // exactly where they yielded).
        let mut cached_code: *const Vec<Op>;
        let mut cached_constants: *const Vec<Value>;
        let mut ip: usize;
        // Flat base of the current frame's stack window. Every slot op adds
        // it to the op's slot index; cached here (re-synced by `re_cache!`)
        // so a slot op does not re-walk `self.frames` per execution.
        let mut cached_base: usize;
        // Coverage: parallel span-id array + resolved file index for the
        // current frame's chunk (`usize::MAX` = skip: unattributed chunk
        // or coverage disabled). Re-resolved per frame change; the per-op
        // hook then costs one integer compare when disabled.
        let mut cached_cov_ids: *const Vec<u32>;
        let mut cached_cov_file: usize;
        // Parallel op spans + this frame's source label for debugger
        // breakpoints (same caching discipline as coverage).
        let mut cached_spans: *const Vec<Span>;
        let mut cached_dbg_label: Option<Arc<str>>;
        // Branch ids parallel to `code` (`COV_UNMAPPED` = not a branch).
        // Recording goes through `record_branch!` inside the five
        // conditional handlers below.
        let mut cached_cov_branch: *const Vec<u32>;
        // Instrumentation gate: `true` when this frame records coverage or a
        // debugger session is attached. The per-op prelude then costs one
        // flag test in the steady state (neither hook active) instead of a
        // coverage compare plus a debugger discriminant check per op; each
        // hook re-checks its own condition inside the gate.
        let mut hooks_on: bool;

        // Re-cache from the current top frame (after any frame push/pop).
        macro_rules! re_cache {
            () => {{
                let f = self.frames.last().unwrap();
                let c = unsafe { &*Arc::as_ptr(&f.chunk) };
                cached_code = &c.code;
                cached_constants = &c.constants;
                ip = f.ip;
                cached_base = f.stack_base;
                cached_cov_ids = &c.cov_ids;
                cached_cov_file = match &interp.coverage {
                    Some(cov) => c
                        .source
                        .as_deref()
                        .and_then(|label| cov.file_index(label))
                        .unwrap_or(usize::MAX),
                    None => usize::MAX,
                };
                cached_cov_branch = &c.cov_branch;
                cached_spans = &c.spans;
                // Only paid when a debugger session is attached: the label
                // is a per-frame `Arc` clone otherwise, and `re_cache!` runs
                // on every call/return. A session attached later (between
                // yields) refreshes this on the resume path.
                cached_dbg_label = if self.debug.is_some() {
                    c.source.clone()
                } else {
                    None
                };
                hooks_on = cached_cov_file != usize::MAX || cached_dbg_label.is_some();
            }};
        }

        // Record one branch direction for the just-executed conditional
        // op at `ip - 1` (`ip` already advanced past it). No-op unless
        // this frame's chunk is attributed. Call before retargeting `ip`.
        macro_rules! record_branch {
            ($taken:expr) => {{
                if cached_cov_file != usize::MAX {
                    let branches: &Vec<u32> = unsafe { &*cached_cov_branch };
                    if let Some(&bid) = branches.get(ip - 1) {
                        if bid != crate::coverage::COV_UNMAPPED {
                            if let Some(cov) = interp.coverage.as_mut() {
                                if let Some(h) = cov.branch_hits.get_mut(bid as usize) {
                                    if $taken {
                                        h.0 += 1;
                                    } else {
                                        h.1 += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }};
        }

        // Green-thread suspension: blocking natives (`chan.recv`,
        // `task.join`) request a yield instead of parking the thread when
        // running on the executor. The call op already completed (its dummy
        // result sits atop the stack for the executor to replace); the
        // frame ip was synced pre-call, so resumption continues right after
        // this op without any re-cache. Only the executor interprets
        // `Flow::Yield`.
        //
        // The executor guard first: `request_yield` fires only on executor
        // threads, so off-executor (plain `zz run`) this is one thread-local
        // cell read instead of a RefCell borrow plus take on every call.
        //
        // `take_yield` itself lives behind a #[cold] boundary so the hot
        // per-op path stays small and I-cache friendly; the inline
        // `on_executor()` check is one thread-local load.
        #[cold]
        fn take_yield_slow() -> Option<crate::value::YieldReason> {
            crate::value::take_yield()
        }
        macro_rules! yield_check {
            () => {{
                if crate::value::on_executor() {
                    if let Some(reason) = take_yield_slow() {
                        return Ok(Flow::Yield(reason));
                    }
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
                let sb = cached_base;
                let f = self.frames.last().unwrap();
                // Sync promoted top-level slots back into the environment so
                // later chunks (REPL statements, other modules) can read them.
                // Do this BEFORE popping the chunk result, since a `Keep`
                // declaration's value may live in a promoted slot.
                for (name, slot) in &f.chunk.toplevel_slots {
                    let idx = sb + *slot as usize;
                    if idx < self.stack.len() {
                        let val = self.stack[idx].clone();
                        interp.env.define(name, val);
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

                let defers: Vec<Value> = std::mem::take(&mut self.defer_stack);
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
            if STATS {
                super::stats::record(op);
            }
            // NO frame.ip write-back here — ip lives in a register.
            // Instrumentation: one flag test in the steady state (no
            // coverage, no debugger). Coverage then costs one integer
            // compare; the debugger one discriminant check.
            if hooks_on {
                if cached_cov_file != usize::MAX {
                    let ids: &Vec<u32> = unsafe { &*cached_cov_ids };
                    if let Some(&id) = ids.get(ip - 1) {
                        if id != crate::coverage::COV_UNMAPPED {
                            if let Some(cov) = interp.coverage.as_mut() {
                                if let Some(h) = cov.hits.get_mut(id as usize) {
                                    *h += 1;
                                }
                            }
                        }
                    }
                }

                // Debugger breakpoints: attributed ops resolve (label, line)
                // through the session and suspend on a line-change hit with
                // the frame ip rewound to the breaking op (`ip` already
                // advanced past it), so resume_chunk executes it instead of
                // skipping it, with the environment intact for inspection.
                if self.debug.is_some() {
                    let spans: &Vec<Span> = unsafe { &*cached_spans };
                    let depth = self.frames.len();
                    let hit = spans.get(ip - 1).and_then(|span| {
                        self.debug.as_ref().and_then(|dbg| {
                            dbg.lock().ok().and_then(|mut session| {
                                session.check_span(cached_dbg_label.as_deref(), *span, depth)
                            })
                        })
                    });
                    if let Some(line) = hit {
                        self.frames.last_mut().unwrap().ip = ip - 1;
                        return Ok(Flow::Yield(crate::value::YieldReason::DebugBreak { line }));
                    }
                }
            }

            match op {
                Op::PushConst(i) => {
                    let v = constants[*i as usize].clone();
                    self.stack.push(v);
                }
                Op::Pop => {
                    self.stack.pop();
                }
                Op::Swap => {
                    let len = self.stack.len();
                    self.stack.swap(len - 1, len - 2);
                }
                Op::Truthy => {
                    let v = self.stack.pop().unwrap();
                    self.stack.push(Value::Bool(v.is_truthy()));
                }
                Op::LoadVar(name, span) => {
                    let v = Self::load_var(interp, name, *span)?;
                    self.stack.push(v);
                }
                Op::LoadPath(parts, span) => {
                    // Unit-variant value (`Token.Eof`): resolve against
                    // the enum table before the value lookup (which
                    // would report "undefined variable" for a type name).
                    // Yields to real values on collision.
                    let v = if parts.len() >= 2 {
                        let head = parts[..parts.len() - 1].join(".");
                        // Miss-only: a shadowing value keeps its meaning.
                        let head_is_value =
                            interp.env.get(&head).is_some() || interp.funcs.contains_key(&head);
                        let canonical = if head_is_value {
                            String::new()
                        } else if interp.enums.contains_key(&head) {
                            head.clone()
                        } else {
                            interp
                                .import_aliases
                                .get(&head)
                                .filter(|q| interp.enums.contains_key(q.as_str()))
                                .cloned()
                                .unwrap_or_default()
                        };
                        if !canonical.is_empty() {
                            let variant = parts.last().cloned().unwrap_or_default();
                            match interp.enums.get(&canonical).and_then(|vs| {
                                vs.iter().find(|(v, _)| v == &variant).map(|(_, has)| *has)
                            }) {
                                // Payload variants need call form; fall
                                // through to the value lookup so the error
                                // names the path, not the enum.
                                Some(true) | None => None,
                                Some(false) => Some(Value::Object(std::sync::Arc::new(
                                    crate::value::ObjectValue {
                                        name: format!("{canonical}.{variant}"),
                                        fields: Vec::new(),
                                    },
                                ))),
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    match v {
                        Some(v) => self.stack.push(v),
                        None => {
                            let v = interp.resolve_path_value(parts, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::DefineVar(name) => {
                    let v = self.stack.pop().unwrap();
                    interp.env.define(name, v.clone());
                    self.stack.push(v);
                }
                Op::StoreVar(name, span) => {
                    let v = self.stack.pop().unwrap();
                    if !interp.env.assign(name, v) {
                        return Err(self.error(format!("undefined variable `{name}`"), *span));
                    }
                }
                Op::StorePath(parts, span) => {
                    let v = self.stack.pop().unwrap();
                    interp.assign_path(parts, v, *span)?;
                }
                Op::LoadSlot(slot) => {
                    let base = cached_base;
                    let v = self.stack[base + *slot as usize].clone();
                    self.stack.push(v);
                }
                Op::StoreSlot(slot) => {
                    let v = self.stack.pop().unwrap();
                    let base = cached_base;
                    self.stack[base + *slot as usize] = v;
                }
                Op::TakeSlot(slot) => {
                    let base = cached_base;
                    let v = std::mem::replace(&mut self.stack[base + *slot as usize], Value::Unit);
                    self.stack.push(v);
                }
                Op::TakeVar(name, span) => {
                    // Move out of the env (no clone); falls back to the
                    // `LoadVar` chain for non-env bindings.
                    if let Some(v) = interp.env.take(name) {
                        self.stack.push(v);
                    } else {
                        let v = Self::load_var_fallback(interp, name, *span)?;
                        self.stack.push(v);
                    }
                }
                Op::VecPush { home, span } => {
                    let elem = self.stack.pop().unwrap();
                    let Some(taken) = Self::take_home(self, interp, home) else {
                        self.stack.push(elem);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    let mut vs = match taken {
                        Value::Array(b) => {
                            std::sync::Arc::try_unwrap(b).unwrap_or_else(|a| (*a).clone())
                        }
                        other => {
                            let msg = format!("`vec.push` expects an array, found `{other}`");
                            Self::restore_home(self, interp, home, other);
                            self.stack.push(elem);
                            return Err(self.error(msg, *span));
                        }
                    };
                    vs.push(elem);
                    Self::restore_home(self, interp, home, Value::Array(std::sync::Arc::new(vs)));
                }
                Op::VecPushField { home, field, span } => {
                    let elem = self.stack.pop().unwrap();
                    let Some(mut taken) = Self::take_home(self, interp, home) else {
                        self.stack.push(elem);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    // `field_push_take` restores the field itself on type
                    // errors; the home object stays intact either way.
                    match Self::field_push_take(&mut taken, field, elem, *span) {
                        Ok(()) => {}
                        Err(msg) => {
                            Self::restore_home(self, interp, home, taken);
                            return Err(self.error(msg, *span));
                        }
                    }
                    Self::restore_home(self, interp, home, taken);
                }
                Op::VecPushMethod { home, method, span } => {
                    let elem = self.stack.pop().unwrap();
                    let Some(taken) = Self::take_home(self, interp, home) else {
                        self.stack.push(elem);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    match taken {
                        Value::Array(b) => {
                            let mut vs =
                                std::sync::Arc::try_unwrap(b).unwrap_or_else(|a| (*a).clone());
                            vs.push(elem);
                            Self::restore_home(
                                self,
                                interp,
                                home,
                                Value::Array(std::sync::Arc::new(vs)),
                            );
                        }
                        other => {
                            // Not an array: restore first (no transient
                            // during user code), then the generic method
                            // call with write-back. The call runs
                            // synchronously via `Interp::call` (same path
                            // the tree-walker uses): `Vm::call_value`
                            // only *pushes* a callee frame for user funcs
                            // and returns, so awaiting a result on the
                            // stack here would read a live frame's slots.
                            Self::restore_home(self, interp, home, other.clone());
                            let (f, recv) = interp.lookup_method_recv(&other, method, *span)?;
                            let arg_vals = vec![recv, elem];
                            let result = interp.call(f, arg_vals, *span).map_err(|mut e| {
                                e.backtrace.extend(self.backtrace());
                                e
                            })?;
                            Self::restore_home(self, interp, home, result);
                            yield_check!();
                        }
                    }
                }
                Op::SetFieldTake { home, field, span } => {
                    let value = self.stack.pop().unwrap();
                    let Some(mut taken) = Self::take_home(self, interp, home) else {
                        self.stack.push(value);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    if let Err(e) = set_object_field(&mut taken, field, value, *span) {
                        Self::restore_home(self, interp, home, taken);
                        return Err(e);
                    }
                    Self::restore_home(self, interp, home, taken);
                }
                Op::SetFieldIndexTake { home, field, span } => {
                    let value = self.stack.pop().unwrap();
                    let index = self.stack.pop().unwrap();
                    let Some(mut taken) = Self::take_home(self, interp, home) else {
                        self.stack.push(index);
                        self.stack.push(value);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    // Take the field out of the owned object (no field
                    // clone); non-direct shapes read generically (same
                    // errors, one field clone — identical to unfused).
                    // `set_index` only writes on success, so on error
                    // `field_val` is untouched and writing it back
                    // restores the exact pre-op object.
                    let mut field_val = match &mut taken {
                        Value::Object(o)
                            if o.fields.iter().any(|(n, _)| n.as_ref() == field.as_str()) =>
                        {
                            let o = std::sync::Arc::make_mut(o);
                            let slot = o
                                .fields
                                .iter_mut()
                                .find(|(n, _)| n.as_ref() == field.as_str())
                                .expect("direct field re-resolved");
                            std::mem::replace(&mut slot.1, Value::Unit)
                        }
                        _ => match object_field(&taken, field, *span) {
                            Ok(v) => v,
                            Err(e) => {
                                Self::restore_home(self, interp, home, taken);
                                return Err(e);
                            }
                        },
                    };
                    if let Err(e) = set_index(&mut field_val, &index, value, *span) {
                        let _ = set_object_field(&mut taken, field, field_val, *span);
                        Self::restore_home(self, interp, home, taken);
                        return Err(e);
                    }
                    if let Err(e) = set_object_field(&mut taken, field, field_val, *span) {
                        Self::restore_home(self, interp, home, taken);
                        return Err(e);
                    }
                    Self::restore_home(self, interp, home, taken);
                }
                Op::FieldBinopTake {
                    home,
                    field,
                    op,
                    span,
                } => {
                    let rhs = self.stack.pop().unwrap();
                    let Some(mut taken) = Self::take_home(self, interp, home) else {
                        self.stack.push(rhs);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    // Read the field generically (one field clone, same as
                    // unfused — the container take is where the Doc
                    // clone dies). `eval_binary` consumes its operands,
                    // but the clone keeps `taken` pristine, so on error
                    // restoring it yields the exact pre-op object.
                    let field_val = match object_field(&taken, field, *span) {
                        Ok(v) => v,
                        Err(e) => {
                            Self::restore_home(self, interp, home, taken);
                            return Err(e);
                        }
                    };
                    let new = match eval_binary(*op, field_val, rhs, *span) {
                        Ok(v) => v,
                        Err(e) => {
                            Self::restore_home(self, interp, home, taken);
                            return Err(e);
                        }
                    };
                    if let Err(e) = set_object_field(&mut taken, field, new, *span) {
                        Self::restore_home(self, interp, home, taken);
                        return Err(e);
                    }
                    Self::restore_home(self, interp, home, taken);
                }
                Op::GetFieldIndex { home, field, span } => {
                    let index = self.stack.pop().unwrap();
                    let Some(taken) = Self::take_home(self, interp, home) else {
                        self.stack.push(index);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    // Direct array/tuple/dict fields: clone one element,
                    // not the field. Anything else completes generically
                    // on the owned object (same calls, same errors).
                    let direct = match &taken {
                        Value::Object(o) => o
                            .fields
                            .iter()
                            .find(|(n, _)| n.as_ref() == field.as_str())
                            .map(|(_, v)| v),
                        _ => None,
                    };
                    let elem = match (direct, &index) {
                        (Some(Value::Array(items)), Value::Int(i)) => {
                            match normalize_index(*i, items.len(), *span) {
                                Ok(ix) => Some(items[ix].clone()),
                                Err(e) => {
                                    Self::restore_home(self, interp, home, taken);
                                    return Err(e);
                                }
                            }
                        }
                        (Some(Value::Tuple(items)), Value::Int(i)) => {
                            match normalize_index(*i, items.len(), *span) {
                                Ok(ix) => Some(items[ix].clone()),
                                Err(e) => {
                                    Self::restore_home(self, interp, home, taken);
                                    return Err(e);
                                }
                            }
                        }
                        (Some(Value::Dict(dict)), key) => dict.get(key).cloned(),
                        _ => None,
                    };
                    match elem {
                        Some(v) => {
                            Self::restore_home(self, interp, home, taken);
                            self.stack.push(v);
                        }
                        None => {
                            match object_field(&taken, field, *span)
                                .and_then(|f| get_index(&f, &index, *span))
                            {
                                Ok(v) => {
                                    Self::restore_home(self, interp, home, taken);
                                    self.stack.push(v);
                                }
                                Err(e) => {
                                    Self::restore_home(self, interp, home, taken);
                                    return Err(e);
                                }
                            }
                        }
                    }
                }
                Op::GetFieldSlotIdx {
                    home,
                    idx,
                    dst,
                    span,
                } => {
                    let Some(taken) = Self::take_home(self, interp, home) else {
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    // Copy the field straight into dst — no container clone,
                    // no stack traffic, fused with the assignment destination.
                    let elem = match &taken {
                        Value::Object(o) => {
                            if (*idx as usize) < o.fields.len() {
                                o.fields[*idx as usize].1.clone()
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
                    };
                    Self::restore_home(self, interp, home, taken);
                    self.stack[cached_base + *dst as usize] = elem;
                }
                Op::LenField { home, field, span } => {
                    let Some(taken) = Self::take_home(self, interp, home) else {
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    // Direct fields: length without cloning anything
                    // (the borrow ends before `taken` moves again).
                    // Anything else completes generically on the owned
                    // object (same errors as `len`).
                    enum LenFast {
                        Hit(i64),
                        Miss,
                        Err(EvalError),
                    }
                    let fast = match &taken {
                        Value::Object(o) => {
                            match o.fields.iter().find(|(n, _)| n.as_ref() == field.as_str()) {
                                Some((_, v)) => match Self::value_len(v) {
                                    Some(n) => LenFast::Hit(n),
                                    None => LenFast::Err(Self::len_error(v, *span)),
                                },
                                None => LenFast::Miss,
                            }
                        }
                        _ => LenFast::Miss,
                    };
                    match fast {
                        LenFast::Hit(n) => {
                            Self::restore_home(self, interp, home, taken);
                            self.stack.push(Value::Int(n));
                        }
                        LenFast::Err(e) => {
                            Self::restore_home(self, interp, home, taken);
                            return Err(e);
                        }
                        LenFast::Miss => match object_field(&taken, field, *span) {
                            Ok(f) => match Self::value_len(&f) {
                                Some(n) => {
                                    Self::restore_home(self, interp, home, taken);
                                    self.stack.push(Value::Int(n));
                                }
                                None => {
                                    Self::restore_home(self, interp, home, taken);
                                    return Err(Self::len_error(&f, *span));
                                }
                            },
                            Err(e) => {
                                Self::restore_home(self, interp, home, taken);
                                return Err(e);
                            }
                        },
                    }
                }
                Op::SlotAddInt { dst, src } => {
                    let base = cached_base;
                    let idx_dst = base + *dst as usize;
                    let idx_src = base + *src as usize;
                    match (&self.stack[idx_dst], &self.stack[idx_src]) {
                        (Value::Int(a), Value::Int(b)) => {
                            let v = Value::Int(a.wrapping_add(*b));
                            // SAFETY: `idx_dst` was just matched as `Int`.
                            unsafe { overwrite_scalar(&mut self.stack[idx_dst], v) };
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
                    let base = cached_base;
                    let idx = base + *slot as usize;
                    match &self.stack[idx] {
                        Value::Int(a) => {
                            let v = Value::Int(a.wrapping_add(1));
                            // SAFETY: `idx` was just matched as `Int`.
                            unsafe { overwrite_scalar(&mut self.stack[idx], v) };
                        }
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
                    let base = cached_base;
                    let idx = base + *dst as usize;
                    match &self.stack[idx] {
                        Value::Int(a) => {
                            let v = Value::Int(a.wrapping_add(*imm));
                            // SAFETY: `idx` was just matched as `Int`.
                            unsafe { overwrite_scalar(&mut self.stack[idx], v) };
                        }
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
                Op::SlotSubIntImm { dst, imm } => {
                    let base = cached_base;
                    let idx = base + *dst as usize;
                    match &self.stack[idx] {
                        Value::Int(a) => {
                            let v = Value::Int(a.wrapping_sub(*imm));
                            // SAFETY: `idx` was just matched as `Int`.
                            unsafe { overwrite_scalar(&mut self.stack[idx], v) };
                        }
                        _ => {
                            let v = self.stack[idx].clone();
                            let span = Span::default();
                            let r = eval_binary(
                                zz_frontend::ast::BinOp::Sub,
                                v,
                                Value::Int(*imm),
                                span,
                            )?;
                            self.stack[idx] = r;
                        }
                    }
                }
                Op::SlotMulIntImm { dst, imm } => {
                    let base = cached_base;
                    let idx = base + *dst as usize;
                    match &self.stack[idx] {
                        Value::Int(a) => {
                            let v = Value::Int(a.wrapping_mul(*imm));
                            // SAFETY: `idx` was just matched as `Int`.
                            unsafe { overwrite_scalar(&mut self.stack[idx], v) };
                        }
                        _ => {
                            let v = self.stack[idx].clone();
                            let span = Span::default();
                            let r = eval_binary(
                                zz_frontend::ast::BinOp::Mul,
                                v,
                                Value::Int(*imm),
                                span,
                            )?;
                            self.stack[idx] = r;
                        }
                    }
                }
                Op::SlotLessIntSlot { a, b } => {
                    let base = cached_base;
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
                    let base = cached_base;
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
                Op::SlotCmpIntSlot { a, b, op } => {
                    let base = cached_base;
                    let idx_a = base + *a as usize;
                    let idx_b = base + *b as usize;
                    match (&self.stack[idx_a], &self.stack[idx_b]) {
                        (Value::Int(x), Value::Int(y)) => match int_cmp(*op, *x, *y) {
                            Some(c) => self.stack.push(Value::Bool(c)),
                            // Not a comparison: the compiler never emits one
                            // here, but IR payloads can; keep the generic
                            // result (an `Int` for arithmetic) instead of
                            // erroring on a valid op.
                            None => {
                                let v = eval_int_binary(*op, *x, *y, Span::default())?;
                                self.stack.push(v);
                            }
                        },
                        _ => {
                            let (l, r) = (self.stack[idx_a].clone(), self.stack[idx_b].clone());
                            let v = eval_binary(*op, l, r, Span::default())?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::SlotCmpIntImm { a, imm, op } => {
                    let base = cached_base;
                    let idx = base + *a as usize;
                    match &self.stack[idx] {
                        Value::Int(x) => match int_cmp(*op, *x, *imm) {
                            Some(c) => self.stack.push(Value::Bool(c)),
                            None => {
                                let v = eval_int_binary(*op, *x, *imm, Span::default())?;
                                self.stack.push(v);
                            }
                        },
                        _ => {
                            let l = self.stack[idx].clone();
                            let v = eval_binary(*op, l, Value::Int(*imm), Span::default())?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::SlotCmpConst { a, k, op } => {
                    let base = cached_base;
                    let idx = base + *a as usize;
                    let rhs = &constants[*k as usize];
                    // `==`/`!=` are the whole point of this op (the string
                    // chain in a `str_scan`-shaped loop): compare the
                    // `Arc<String>` in place instead of cloning both sides
                    // through `eval_binary`.
                    let fast = match (&self.stack[idx], rhs) {
                        (Value::Str(x), Value::Str(y)) => match op {
                            BinOp::Eq => Some(x == y),
                            BinOp::Ne => Some(x != y),
                            _ => None,
                        },
                        (Value::Int(x), Value::Int(y)) => int_cmp(*op, *x, *y),
                        _ => None,
                    };
                    match fast {
                        Some(c) => self.stack.push(Value::Bool(c)),
                        None => {
                            let l = self.stack[idx].clone();
                            let v = eval_binary(*op, l, rhs.clone(), Span::default())?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::SlotBinaryInt { dst, lhs, rhs, op } => {
                    let base = cached_base;
                    let idx_d = base + *dst as usize;
                    let idx_l = base + *lhs as usize;
                    let idx_r = base + *rhs as usize;
                    match (&self.stack[idx_l], &self.stack[idx_r]) {
                        (Value::Int(a), Value::Int(b)) => {
                            let v = eval_int_binary(*op, *a, *b, Span::default())?;
                            write_slot_checked(&mut self.stack[idx_d], v);
                        }
                        _ => {
                            let (l, r) = (self.stack[idx_l].clone(), self.stack[idx_r].clone());
                            let span = Span::default();
                            let v = eval_binary(*op, l, r, span)?;
                            write_slot_checked(&mut self.stack[idx_d], v);
                        }
                    }
                }
                Op::SlotBinaryIntImm { dst, lhs, imm, op } => {
                    let base = cached_base;
                    let idx_d = base + *dst as usize;
                    let idx_l = base + *lhs as usize;
                    match &self.stack[idx_l] {
                        Value::Int(a) => {
                            let v = eval_int_binary(*op, *a, *imm, Span::default())?;
                            write_slot_checked(&mut self.stack[idx_d], v);
                        }
                        _ => {
                            let l = self.stack[idx_l].clone();
                            let span = Span::default();
                            let v = eval_binary(*op, l, Value::Int(*imm), span)?;
                            write_slot_checked(&mut self.stack[idx_d], v);
                        }
                    }
                }
                Op::JumpIfFalseSlotCmpIntImm {
                    a,
                    imm,
                    op,
                    target,
                    span,
                } => {
                    let idx = cached_base + *a as usize;
                    let cond = match &self.stack[idx] {
                        Value::Int(x) => match int_cmp(*op, *x, *imm) {
                            Some(c) => Value::Bool(c),
                            None => eval_int_binary(*op, *x, *imm, *span)?,
                        },
                        _ => {
                            let l = self.stack[idx].clone();
                            eval_binary(*op, l, Value::Int(*imm), *span)?
                        }
                    };
                    let jumped = match cond {
                        Value::Bool(b) => !b,
                        // Same contract as `JumpIfFalseBool`, which this op
                        // replaces.
                        _ => return Err(self.error("`if` condition must be a bool", *span)),
                    };
                    record_branch!(jumped);
                    if jumped {
                        ip = *target;
                    }
                }
                Op::JumpIfFalseSlotCmpIntSlot {
                    a,
                    b,
                    op,
                    target,
                    span,
                } => {
                    let idx_a = cached_base + *a as usize;
                    let idx_b = cached_base + *b as usize;
                    let cond = match (&self.stack[idx_a], &self.stack[idx_b]) {
                        (Value::Int(x), Value::Int(y)) => match int_cmp(*op, *x, *y) {
                            Some(c) => Value::Bool(c),
                            None => eval_int_binary(*op, *x, *y, *span)?,
                        },
                        _ => {
                            let (l, r) = (self.stack[idx_a].clone(), self.stack[idx_b].clone());
                            eval_binary(*op, l, r, *span)?
                        }
                    };
                    let jumped = match cond {
                        Value::Bool(b) => !b,
                        _ => return Err(self.error("`if` condition must be a bool", *span)),
                    };
                    record_branch!(jumped);
                    if jumped {
                        ip = *target;
                    }
                }
                Op::PushSlotIntImm { a, imm, op, span } => {
                    let idx = cached_base + *a as usize;
                    let v = match &self.stack[idx] {
                        Value::Int(l) => eval_int_binary(*op, *l, *imm, *span)?,
                        other => eval_binary(*op, other.clone(), Value::Int(*imm), *span)?,
                    };
                    self.stack.push(v);
                }
                Op::MakeFunc {
                    name,
                    params,
                    chunk: fchunk,
                    defaults,
                } => {
                    let fv = FuncValue {
                        params: params.clone(),
                        body: Expr::Block(Block {
                            stmts: Vec::new(),
                            span: Span::new(0, 0),
                        }),
                        env: interp.env.clone(),
                        chunk: Some(Arc::clone(fchunk)),
                        chunk_defaults: defaults.clone(),
                        // Named function (`func`): explicit `return` only.
                        tail_value: false,
                    };
                    let fv = std::rc::Rc::new(fv);
                    interp.funcs.insert(name.clone(), std::rc::Rc::clone(&fv));
                    interp.funcs_version = interp.funcs_version.wrapping_add(1);
                    interp.env.define(name, Value::Func(fv));
                    self.stack.push(Value::Unit);
                }
                Op::RegisterStruct { name, fields } => {
                    // Copy-on-write (see tree-walker `Stmt::Struct`).
                    Arc::make_mut(&mut interp.structs).insert(name.clone(), fields.clone());
                    self.stack.push(Value::Unit);
                }
                Op::RegisterEnum { name, variants } => {
                    Arc::make_mut(&mut interp.enums).insert(name.clone(), variants.clone());
                    self.stack.push(Value::Unit);
                }
                Op::MakeEnum {
                    enum_name,
                    variant,
                    argc,
                    span,
                } => {
                    // Miss-only alias fallback (mirrors `MakeStruct`):
                    // bare `Token` from `import m(Token)` resolves to
                    // `m.Token`. Seed entries take precedence.
                    let resolved;
                    let lookup = if interp.enums.contains_key(enum_name) {
                        enum_name
                    } else if let Some(qualified) = interp.import_aliases.get(enum_name) {
                        resolved = qualified.clone();
                        &resolved
                    } else {
                        return Err(self.error(format!("unknown enum `{enum_name}`"), *span));
                    };
                    let Some(variants) = interp.enums.get(lookup).cloned() else {
                        return Err(self.error(format!("unknown enum `{enum_name}`"), *span));
                    };
                    let has_payload = match variants.iter().find(|(v, _)| v == variant) {
                        Some((_, has)) => *has,
                        None => {
                            return Err(self.error(
                                format!("unknown variant `{variant}` for enum `{lookup}`"),
                                *span,
                            ));
                        }
                    };
                    let mut vals = Vec::with_capacity(*argc as usize);
                    for _ in 0..*argc {
                        vals.push(self.stack.pop().unwrap());
                    }
                    vals.reverse();
                    // Arity is a checker error; the runtime keeps a
                    // defensive gate so unchecked paths never silently
                    // build a wrong-shaped value.
                    let fields = match (has_payload, vals.len()) {
                        (false, 0) => Vec::new(),
                        (true, 1) => {
                            vec![(
                                std::sync::Arc::from("value"),
                                vals.into_iter().next().unwrap(),
                            )]
                        }
                        _ => {
                            return Err(self.error(
                                format!(
                                    "variant `{lookup}.{variant}` takes {} argument{} but {} given",
                                    if has_payload { 1 } else { 0 },
                                    if has_payload { "" } else { "s" },
                                    vals.len(),
                                ),
                                *span,
                            ));
                        }
                    };
                    self.stack.push(Value::Object(std::sync::Arc::new(
                        crate::value::ObjectValue {
                            name: format!("{lookup}.{variant}"),
                            fields,
                        },
                    )));
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
                        // Wrapping add in every profile (IR spec).
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Int(a.wrapping_add(*b)));
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
                        // Wrapping sub in every profile (IR spec).
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Int(a.wrapping_sub(*b)));
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
                        // Wrapping mul in every profile (IR spec).
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Int(a.wrapping_mul(*b)));
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
                            // MIN/-1 traps in every profile (IR spec);
                            // zero is rejected above.
                            if *a == i64::MIN && *b == -1 {
                                return Err(EvalError::new("integer overflow in division", *span));
                            }
                            self.stack.push(Value::Int(a.wrapping_div(*b)));
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
                            // MIN%-1 traps in every profile (IR spec);
                            // zero is rejected above.
                            if *a == i64::MIN && *b == -1 {
                                return Err(EvalError::new("integer overflow in modulo", *span));
                            }
                            self.stack.push(Value::Int(a.wrapping_rem(*b)));
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
                        // Wrapping negation in every profile (IR spec).
                        Value::Int(a) => {
                            self.stack.push(Value::Int(a.wrapping_neg()));
                        }
                        _ => {
                            let v = eval_unary(zz_frontend::ast::UnOp::Neg, v, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                // Typed int comparisons (#364): inline `(Int, Int)`
                // (plain i64 semantics, exactly like `eval_binary`);
                // anything else keeps generic dispatch.
                Op::IntLt(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Bool(a < b));
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Lt, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntGt(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Bool(a > b));
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Gt, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntLe(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Bool(a <= b));
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Le, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntGe(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Bool(a >= b));
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Ge, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntEq(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Bool(a == b));
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Eq, l, r, *span)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::IntNe(span) => {
                    let r = self.stack.pop().unwrap();
                    let l = self.stack.pop().unwrap();
                    match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            self.stack.push(Value::Bool(a != b));
                        }
                        _ => {
                            let v = eval_binary(zz_frontend::ast::BinOp::Ne, l, r, *span)?;
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
                    if self.safepoint_expired() {
                        self.frames.last_mut().unwrap().ip = ip;
                        return Ok(Flow::Yield(crate::value::YieldReason::Timeslice));
                    }
                }
                Op::JumpIfFalse(target) => {
                    let v = self.stack.pop().unwrap();
                    let jumped = !v.is_truthy();
                    record_branch!(jumped);
                    if jumped {
                        ip = *target;
                    }
                }
                Op::JumpIfTrue(target) => {
                    let v = self.stack.pop().unwrap();
                    let jumped = v.is_truthy();
                    record_branch!(jumped);
                    if jumped {
                        ip = *target;
                    }
                }
                Op::JumpIfFalseBool(target, span) => {
                    let v = self.stack.pop().unwrap();
                    if !matches!(v, Value::Bool(_)) {
                        return Err(self.error("`if` condition must be a bool", *span));
                    }
                    let jumped = !v.is_truthy();
                    record_branch!(jumped);
                    if jumped {
                        ip = *target;
                    }
                }
                Op::Return => {
                    let v = self.stack.pop().unwrap();
                    let defers: Vec<Value> = std::mem::take(&mut self.defer_stack);
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
                        Value::Bytes(_) => (it, Value::Int(0)),
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
                        env: interp.env.clone(),
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
                    vars,
                    exit,
                    body,
                    in_env,
                    span,
                } => {
                    // Cooperative safepoint for `for` loops, folded in from
                    // the header (see the `Stmt::For` arm in `compiler.rs`):
                    // this op is the loop header target for both the
                    // back-edge and `continue`, so the check covers every
                    // iteration without a dispatch of its own. Taken before
                    // any iteration state is mutated so a yield can rewind
                    // to this op and re-execute it whole.
                    if self.safepoint_expired() {
                        self.frames.last_mut().unwrap().ip = ip - 1;
                        return Ok(Flow::Yield(crate::value::YieldReason::Timeslice));
                    }
                    // In-place fast paths. `for i in a..b` and `for x in xs`
                    // — the shapes nearly every index/element loop compiles
                    // to — leave `[iterable, idx, var]` on top of the stack,
                    // so the generic churn below (truncate the var, pop the
                    // index as a `Value`, match the iterable, push idx + var
                    // back) is a compare and two in-place writes. Both edges
                    // end in the exact state the generic path ends in: a live
                    // iteration leaves the next index under the new var, and
                    // the exhausted edge pops the loop frame and truncates to
                    // its base, so the caller's `Pop` still drops the
                    // iterable. `in_env` loops (the var lives in a scope),
                    // multi-var loops and every other iterable (dict,
                    // `enumerate()` pairs) stay generic.
                    //
                    // The window is split so the entry can be read while the
                    // var is written (`head` ends at the index, `tail` is
                    // `[idx, var]`). Nothing is staged in a temporary: an
                    // `Option<(Value, Value)>` here copies two `Value`s per
                    // iteration and measured -21% on `loop_sum`.
                    let fast: Option<bool> = {
                        let len = self.stack.len();
                        let mut step = None;
                        if !*in_env && vars.len() == 1 && len >= 3 {
                            let (head, tail) = self.stack.split_at_mut(len - 2);
                            if let &Value::Int(i) = &tail[0] {
                                step = match &head[len - 3] {
                                    Value::Range(r) => {
                                        let (s, end) = (r.step, r.end);
                                        if (s > 0 && i >= end) || (s <= 0 && i <= end) {
                                            Some(true)
                                        } else {
                                            // SAFETY: `tail[0]` was matched as
                                            // `Int` above; `tail[1]` is the
                                            // loop var, checked scalar.
                                            unsafe {
                                                overwrite_scalar(
                                                    &mut tail[0],
                                                    Value::Int(i.wrapping_add(s)),
                                                )
                                            };
                                            write_slot_checked(&mut tail[1], Value::Int(i));
                                            Some(false)
                                        }
                                    }
                                    Value::Array(arr) => {
                                        if i >= arr.len() as i64 {
                                            Some(true)
                                        } else {
                                            // Clone while the entry borrows
                                            // the front of the window.
                                            tail[1] = arr[i as usize].clone();
                                            // SAFETY: `tail[0]` was matched
                                            // as `Int` above.
                                            unsafe {
                                                overwrite_scalar(
                                                    &mut tail[0],
                                                    Value::Int(i.wrapping_add(1)),
                                                )
                                            };
                                            Some(false)
                                        }
                                    }
                                    Value::Bytes(b) => {
                                        if i >= b.len() as i64 {
                                            Some(true)
                                        } else {
                                            write_slot_checked(
                                                &mut tail[1],
                                                Value::Int(b.as_slice()[i as usize] as i64),
                                            );
                                            // SAFETY: `tail[0]` was matched
                                            // as `Int` above.
                                            unsafe {
                                                overwrite_scalar(
                                                    &mut tail[0],
                                                    Value::Int(i.wrapping_add(1)),
                                                )
                                            };
                                            Some(false)
                                        }
                                    }
                                    _ => None,
                                };
                            }
                        }
                        step
                    };
                    match fast {
                        Some(true) => {
                            record_branch!(true);
                            let li = self.loops.pop().unwrap();
                            self.stack.truncate(li.stack_base + 1);
                            interp.env = li.env;
                            ip = *exit;
                            continue;
                        }
                        Some(false) => {
                            record_branch!(false);
                            ip = *body;
                            continue;
                        }
                        None => {}
                    }
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
                                next_idx = Value::Int(i.wrapping_add(step));
                                push_val = Value::Int(i);
                                push_val2 = None;
                            }
                            (Value::Array(arr), Value::Int(i)) => {
                                let i = *i;
                                if i >= arr.len() as i64 {
                                    iter_done = true;
                                    next_idx = Value::Unit;
                                    push_val = Value::Unit;
                                    push_val2 = None;
                                } else {
                                    iter_done = false;
                                    next_idx = Value::Int(i.wrapping_add(1));
                                    let item = arr[i as usize].clone();
                                    if num_vars == 2 {
                                        // `for i, x in xs.enumerate()` —
                                        // each item is a 2-tuple.
                                        // `enumerate()` builds `Tuple`
                                        // values while tuple literals
                                        // evaluate to 2-element arrays —
                                        // accept both.
                                        let pair_vec: Option<Vec<Value>> = match item {
                                            Value::Tuple(pair) => Some(
                                                std::sync::Arc::try_unwrap(pair)
                                                    .unwrap_or_else(|a| (*a).clone()),
                                            ),
                                            Value::Array(items) if items.len() == 2 => Some(
                                                std::sync::Arc::try_unwrap(items)
                                                    .unwrap_or_else(|a| (*a).clone()),
                                            ),
                                            _ => None,
                                        };
                                        match pair_vec {
                                            Some(mut pair) => {
                                                let second = pair.pop().unwrap();
                                                let first = pair.pop().unwrap();
                                                push_val = first;
                                                push_val2 = Some(second);
                                            }
                                            None => {
                                                return Err(self.error(
                                                    "cannot unpack a non-pair value into 2 loop variables (expected `enumerate()` pairs)",
                                                    *span,
                                                ));
                                            }
                                        }
                                    } else {
                                        push_val = item;
                                        push_val2 = None;
                                    }
                                }
                            }
                            (Value::Bytes(b), Value::Int(i)) => {
                                let i = *i;
                                if i >= b.len() as i64 {
                                    iter_done = true;
                                    next_idx = Value::Unit;
                                    push_val = Value::Unit;
                                } else {
                                    iter_done = false;
                                    next_idx = Value::Int(i.wrapping_add(1));
                                    push_val = Value::Int(b.as_slice()[i as usize] as i64);
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
                                    next_idx = Value::Int((i as i64).wrapping_add(1));
                                    push_val = pairs.entries()[i].0.clone();
                                    if num_vars == 2 {
                                        push_val2 = Some(pairs.entries()[i].1.clone());
                                    } else {
                                        push_val2 = None;
                                    }
                                }
                            }
                            _ => {
                                return Err(self.error("for-loop over a non-iterable value", *span))
                            }
                        }
                    } // immutable borrow of self.stack dropped here

                    if iter_done {
                        record_branch!(true);
                        let li = self.loops.pop().unwrap();
                        self.stack.truncate(li.stack_base + 1);
                        interp.env = li.env;
                        ip = *exit;
                    } else {
                        record_branch!(false);
                        self.stack.push(next_idx);
                        self.stack.push(push_val.clone());
                        if let Some(ref v) = push_val2 {
                            self.stack.push(v.clone());
                        }
                        if *in_env {
                            let li = self.loops.last().unwrap();
                            let loop_env = li.env.clone();
                            interp.env = loop_env;
                            let mut scope = Env::with_parent(&interp.env);
                            if let Some(ref v2) = push_val2 {
                                scope.define(&vars[0], push_val);
                                scope.define(&vars[1], v2.clone());
                            } else {
                                scope.define(&vars[0], push_val);
                            }
                            interp.env = scope;
                        }
                        // Rotated layout: the body follows in `body` (the
                        // loop is entered at this op), so this jump is the
                        // back-edge — one dispatch per iteration.
                        ip = *body;
                    }
                }
                Op::WhileSetup { exit, header } => {
                    self.loops.push(LoopInfo {
                        exit: *exit,
                        header: *header,
                        env: interp.env.clone(),
                        frame_idx: self.frames.len() - 1,
                        stack_base: self.stack.len(),
                        slots: 0,
                    });
                }
                Op::WhileCond { exit, span } => {
                    // Cooperative safepoint for `while` loops, folded in from
                    // the header (see `Compiler::compile_while`): this op is
                    // reached once per test — entry, back-edge and `continue`
                    // all land on the condition — so the check cannot be
                    // skipped, at one dispatch less per iteration. Taken
                    // before the condition is popped so a yield rewinds to
                    // this op and re-executes it whole.
                    if self.safepoint_expired() {
                        self.frames.last_mut().unwrap().ip = ip - 1;
                        return Ok(Flow::Yield(crate::value::YieldReason::Timeslice));
                    }
                    let c = self.stack.pop().unwrap();
                    if !matches!(c, Value::Bool(_)) {
                        return Err(self.error("`while` condition must be a bool", *span));
                    }
                    let done = !c.is_truthy();
                    record_branch!(done);
                    if done {
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
                    interp.env = li.env.clone();
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
                    self.stack.push(Value::Array(std::sync::Arc::new(items)));
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
                            for item in std::sync::Arc::try_unwrap(items)
                                .unwrap_or_else(|a| (*a).clone())
                                .into_iter()
                                .rev()
                            {
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
                    // Owned pop: mutate in place when uniquely owned.
                    std::sync::Arc::make_mut(&mut arr).push(value);
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
                    self.stack.push(Value::Dict(std::sync::Arc::new(
                        crate::value::DictData::from_entries(pairs),
                    )));
                }
                Op::IndexOp(span) => {
                    let iv = self.stack.pop().unwrap();
                    let ov = self.stack.pop().unwrap();
                    let v = get_index(&ov, &iv, *span)?;
                    self.stack.push(v);
                }
                // Read `array[idx]` where both are stack slots: single bounds
                // check, no `normalize_index` call, no `Value` tag loop.
                Op::IndexIntSlot {
                    arr,
                    idx,
                    dst,
                    span,
                } => {
                    let ov = self.stack[cached_base + *arr as usize].clone();
                    let iv = self.stack[cached_base + *idx as usize].clone();
                    let v = match (&ov, &iv) {
                        (Value::Array(items), Value::Int(i)) => {
                            let ix = normalize_index(*i, items.len(), *span)?;
                            items[ix].clone()
                        }
                        (Value::Tuple(items), Value::Int(i)) => {
                            let ix = normalize_index(*i, items.len(), *span)?;
                            items[ix].clone()
                        }
                        (Value::Bytes(b), Value::Int(i)) => {
                            let ix = normalize_index(*i, b.len(), *span)?;
                            Value::Int(b.as_slice()[ix] as i64)
                        }
                        (Value::Dict(dict), key) => match dict.get_cloned(key) {
                            Some(v) => v,
                            None => {
                                return Err(
                                    self.error(format!("key `{key}` not found in dict"), *span)
                                );
                            }
                        },
                        (Value::Str(s), Value::Int(i)) => {
                            if s.is_ascii() {
                                let ix = normalize_index(*i, s.len(), *span)?;
                                Value::Str(std::sync::Arc::clone(
                                    &crate::runtime::ops::ascii_chars()[s.as_bytes()[ix] as usize],
                                ))
                            } else {
                                let chars: Vec<char> = s.chars().collect();
                                let ix = normalize_index(*i, chars.len(), *span)?;
                                Value::Str(chars[ix].to_string().into())
                            }
                        }
                        _ => {
                            return Err(self.error(
                                format!("cannot index a value of type `{}`", ov.type_name()),
                                *span,
                            ));
                        }
                    };
                    self.stack[cached_base + *dst as usize] = v;
                }
                // Read `dict[key]` where key is an int: probe DictData with
                // a precomputed hash + open-addressed table.
                Op::IndexDictInt {
                    dict,
                    key,
                    dst,
                    span,
                } => {
                    let ov = self.stack[cached_base + *dict as usize].clone();
                    let iv = self.stack[cached_base + *key as usize].clone();
                    let v = match (&ov, &iv) {
                        (Value::Dict(dict), key) => match dict.get_cloned(key) {
                            Some(v) => v,
                            None => {
                                return Err(
                                    self.error(format!("key `{key}` not found in dict"), *span)
                                );
                            }
                        },
                        _ => {
                            return Err(
                                self.error("expected dict for indexed access".to_string(), *span)
                            );
                        }
                    };
                    self.stack[cached_base + *dst as usize] = v;
                }
                Op::StoreIndexOp(span) => {
                    // Stack layout is evaluation order: [object, index,
                    // value] (spec §7: base, index, value, left-to-right).
                    let value = self.stack.pop().unwrap();
                    let iv = self.stack.pop().unwrap();
                    let mut ov = self.stack.pop().unwrap();
                    set_index(&mut ov, &iv, value, *span)?;
                    self.stack.push(ov);
                }
                Op::StoreIndexTake { home, span } => {
                    // Stack layout is [index, value]: the base lives in
                    // the home and only the two operands were evaluated,
                    // so the take window is this instruction alone.
                    let value = self.stack.pop().unwrap();
                    let iv = self.stack.pop().unwrap();
                    let Some(mut ov) = Self::take_home(self, interp, home) else {
                        self.stack.push(iv);
                        self.stack.push(value);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    // `set_index` mutates in place (refcount 1) and only
                    // writes on success, so restoring the taken value puts
                    // back exactly the pre-op container on error and the
                    // updated one on success.
                    let res = set_index(&mut ov, &iv, value, *span);
                    Self::restore_home(self, interp, home, ov);
                    res?;
                }
                Op::CompoundIndexOp { op, span } => {
                    let rhs = self.stack.pop().unwrap();
                    let iv = self.stack.pop().unwrap();
                    let mut ov = self.stack.pop().unwrap();
                    let cur = get_index(&ov, &iv, *span)?;
                    let new = eval_binary(*op, cur, rhs, *span)?;
                    set_index(&mut ov, &iv, new, *span)?;
                    self.stack.push(ov);
                }
                Op::CompoundIndexTake { home, op, span } => {
                    // Stack layout is [index, rhs] (the base lives in the
                    // home), so the take window holds no user code.
                    let rhs = self.stack.pop().unwrap();
                    let iv = self.stack.pop().unwrap();
                    let Some(mut ov) = Self::take_home(self, interp, home) else {
                        self.stack.push(iv);
                        self.stack.push(rhs);
                        return Err(self.error(
                            format!(
                                "undefined variable `{}`",
                                match home {
                                    TakeHome::Slot(_) => String::from("<slot>"),
                                    TakeHome::Env(n) => n.clone(),
                                }
                            ),
                            *span,
                        ));
                    };
                    // Any failure before the write leaves the container
                    // untouched, so restoring it puts back exactly the pre-op
                    // state; a successful write is restored too.
                    let cur = get_index(&ov, &iv, *span);
                    let res = cur.and_then(|cur| {
                        let new = eval_binary(*op, cur, rhs, *span)?;
                        set_index(&mut ov, &iv, new, *span)
                    });
                    Self::restore_home(self, interp, home, ov);
                    res?;
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
                    // Selective-import alias (miss-only): bare `Product`
                    // from `import m(Product)` resolves to `m.Product`.
                    // Seed entries take precedence; this mirrors the
                    // Ident fallback above.
                    let resolved;
                    let lookup = if interp.structs.contains_key(name) {
                        name
                    } else if let Some(qualified) = interp.import_aliases.get(name) {
                        resolved = qualified.clone();
                        &resolved
                    } else {
                        return Err(self.error(format!("unknown struct `{name}`"), *span));
                    };
                    // Borrowed, not cloned: the layout lives in `interp` for
                    // the whole run, and a clone is a `Vec<String>` plus one
                    // `String` per field on every construction.
                    let Some(registered) = interp.structs.get(lookup) else {
                        return Err(self.error(format!("unknown struct `{name}`"), *span));
                    };
                    let mut vals = Vec::with_capacity(field_names.len());
                    for _ in 0..field_names.len() {
                        vals.push(self.stack.pop().unwrap());
                    }
                    vals.reverse();
                    // Fast path: the literal names exactly the struct's own
                    // fields, so none is an embedded (promoted) field.
                    // `build_struct_literal` would then pay a `given` vector,
                    // a `leftovers` filter, a `contains` per field and a
                    // `find` per field — all avoidable once positions are
                    // known. The names come interned from the chunk, so each
                    // instance copies refcounts instead of allocating.
                    let direct = field_names.len() == registered.len()
                        && field_names
                            .iter()
                            .all(|n| registered.iter().any(|r| r.as_str() == n.as_ref()))
                        && registered
                            .iter()
                            .all(|r| field_names.iter().any(|n| n.as_ref() == r.as_str()));
                    let obj = if direct {
                        let mut out: Vec<(std::sync::Arc<str>, Value)> =
                            Vec::with_capacity(registered.len());
                        if field_names
                            .iter()
                            .zip(registered.iter())
                            .all(|(a, b)| a.as_ref() == b.as_str())
                        {
                            // Declared order: pair positionally.
                            for (fname, v) in field_names.iter().zip(vals) {
                                out.push((std::sync::Arc::clone(fname), v));
                            }
                        } else {
                            let mut slots: Vec<Option<Value>> =
                                vals.into_iter().map(Some).collect();
                            for fname in registered.iter() {
                                let pos = field_names
                                    .iter()
                                    .position(|n| n.as_ref() == fname.as_str())
                                    .expect("direct fast path proved membership");
                                let v = slots[pos]
                                    .take()
                                    .expect("direct fast path proved uniqueness");
                                out.push((std::sync::Arc::clone(&field_names[pos]), v));
                            }
                        }
                        crate::value::ObjectValue {
                            name: lookup.to_string(),
                            fields: out,
                        }
                    } else {
                        // Flattened (promoted) fields are distributed into
                        // embedded sub-objects inside `build_struct_literal`.
                        let given: Vec<(std::sync::Arc<str>, Value)> =
                            field_names.iter().cloned().zip(vals).collect();
                        crate::runtime::ops::build_struct_literal(
                            &interp.structs,
                            lookup,
                            registered,
                            &given,
                            *span,
                            0,
                        )?
                    };
                    self.stack.push(Value::Object(std::sync::Arc::new(obj)));
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
                Op::CompoundFieldOp { name, op, span } => {
                    let rhs = self.stack.pop().unwrap();
                    let mut ov = self.stack.pop().unwrap();
                    let cur = object_field(&ov, name, *span)?;
                    let new = eval_binary(*op, cur, rhs, *span)?;
                    set_object_field(&mut ov, name, new, *span)?;
                    self.stack.push(ov);
                }
                Op::SetFieldIdx(idx, span) => {
                    let mut ov = self.stack.pop().unwrap();
                    let value = self.stack.pop().unwrap();
                    match &mut ov {
                        Value::Object(o) => {
                            let o = std::sync::Arc::make_mut(o);
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
                Op::MakeClosure {
                    params,
                    chunk,
                    defaults,
                } => {
                    let fv = FuncValue {
                        params: params.clone(),
                        body: Expr::Block(Block {
                            stmts: Vec::new(),
                            span: Span::new(0, 0),
                        }),
                        env: interp.env.clone(),
                        chunk: Some(Arc::clone(chunk)),
                        chunk_defaults: defaults.clone(),
                        // Closure: body tail is the value.
                        tail_value: true,
                    };
                    self.stack.push(Value::Func(std::rc::Rc::new(fv)));
                }
                Op::SpawnClosure {
                    params,
                    chunk,
                    span,
                    defaults,
                } => {
                    // Fused spawn (see `SpawnHook`): the chunk + params go
                    // straight to the task constructor — no FuncValue box,
                    // no args Vec, no native lookup. Creation env is the
                    // current env, exactly as MakeClosure would capture.
                    let hook = crate::eval::SPAWN_HOOK.get().copied().ok_or_else(|| {
                        self.error("`task.spawn` used without stdlib task support", *span)
                    })?;
                    let v = hook(interp, chunk, params, defaults, *span)?;
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
                        let mut scope = Env::with_parent(&interp.env);
                        let m = Interp::match_pattern(pat, &sv, &mut scope);
                        if m {
                            interp.env = scope;
                        }
                        m
                    } else {
                        Interp::match_pattern(pat, &sv, &mut interp.env)
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
                                if let Some(env_ref) = interp.env.parent_link() {
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
                        let mut scope = Env::with_parent(&interp.env);
                        let m = Interp::match_pattern(pat, &v, &mut scope);
                        if m {
                            interp.env = scope;
                        }
                        m
                    } else {
                        Interp::match_pattern(pat, &v, &mut interp.env)
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
                                        let Some(fv) =
                                            zz_checker::lookup_std(&interp.funcs, &fname).cloned()
                                        else {
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
                                            let callee = Value::Func(std::rc::Rc::clone(&fv));
                                            self.call_value(callee, vec![err_c], span_c, interp)?;
                                            self.try_convert_depths.push(self.frames.len());
                                            re_cache!();
                                        } else {
                                            let callee = Value::Func(std::rc::Rc::clone(&fv));
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
                    // Scratch args buffer (same discipline as `CallNative`):
                    // reuses one allocation per thread instead of one per
                    // call. `call_value` returns the buffer for chunked
                    // callees; slow paths consume it (correct, just slower).
                    let mut args = take_scratch_args();
                    args.reserve(argc as usize);
                    for _ in 0..argc {
                        args.push(self.stack.pop().unwrap());
                    }
                    if args.len() > 1 {
                        args.reverse();
                    }
                    let callee = self.stack.pop().unwrap();
                    // Sync ip back so the parent frame resumes at the right spot.
                    self.frames.last_mut().unwrap().ip = ip;
                    self.call_value(callee, args, span, interp)?;
                    re_cache!();
                    yield_check!();
                }
                Op::CallPath {
                    parts,
                    joined,
                    argc,
                    span,
                    pspan,
                } => {
                    let argc = *argc;
                    let span = *span;
                    let pspan = *pspan;
                    // Direct script-function call: `fib(n)` and module paths
                    // like `lib.fib(n)` resolve to a compiled function bound
                    // under the precomputed `joined` key. Entering its frame
                    // here skips the per-call `Value::Func` clones (the
                    // `is_direct` probe's discarded one included) and every
                    // fallback probe below. Anything this can't prove
                    // equivalent — a shadowing binding, a native, defaults to
                    // fill, a wrong arity — falls through unchanged.
                    self.frames.last_mut().unwrap().ip = ip;
                    // Memoized resolution: this site calls the same function
                    // until the environment or the function table changes, so
                    // the two name probes (`env.bound` + `funcs.get`, ~120 ns
                    // together) only run once per generation.
                    let caller = self.frames.last().map(|f| f.chunk.id).unwrap_or(0);
                    let gen = crate::env::env_gen() ^ interp.funcs_version;
                    if let Some((chunk, env)) = self.direct_call_hit(caller, ip, argc, gen) {
                        // Arguments are already contiguous at the top of the
                        // stack, in parameter order: no scratch buffer, no
                        // pop/push round trip.
                        let base = self.stack.len() - argc as usize;
                        self.open_frame(base, chunk, env, span, interp);
                        re_cache!();
                        yield_check!();
                        continue;
                    }
                    // Miss: the fallbacks below want the arguments off the
                    // stack (they may be reversed, reordered, or handed to a
                    // native), so pay the scratch buffer once.
                    let mut args = take_scratch_args();
                    args.reserve(argc as usize);
                    for _ in 0..argc {
                        args.push(self.stack.pop().unwrap());
                    }
                    if args.len() > 1 {
                        args.reverse();
                    }
                    match self.try_enter_direct_call(
                        joined,
                        args,
                        span,
                        interp,
                        Some((caller, ip, gen)),
                    ) {
                        None => {
                            re_cache!();
                            yield_check!();
                            continue;
                        }
                        Some(back) => args = back,
                    }
                    if parts.len() >= 2 {
                        // Precomputed at compile time (see `Op::CallPath`):
                        // no lookup-string allocation per call.
                        let is_direct = interp.env.get(joined).is_some()
                            || zz_checker::lookup_std(&interp.funcs, joined).is_some()
                            || zz_checker::lookup_std(&interp.natives, joined).is_some();
                        // Fast path (#364): a direct hit skips every
                        // fallback probe below (head-shadow joins, enum
                        // tables, and a second full path resolution) —
                        // identical semantics, one resolution per call.
                        if is_direct {
                            let callee = interp.resolve_path_value(parts, pspan)?;
                            self.frames.last_mut().unwrap().ip = ip;
                            self.call_value(callee, args, span, interp)?;
                            re_cache!();
                            yield_check!();
                            continue;
                        }
                        // Enum construction fallback (untyped compiles,
                        // cross-snippet REPL): the head names an enum and
                        // resolves to no value — build the variant object
                        // directly instead of failing the path lookup.
                        // Miss-only: a shadowing value keeps its
                        // meaning (mirrors the checker and tree-walker).
                        // `head` is built once and reused below (it was
                        // joined three times per call before).
                        let head: Option<String> = if parts.len() >= 2 {
                            Some(parts[..parts.len() - 1].join("."))
                        } else {
                            None
                        };
                        let head_shadowed = head.as_ref().is_some_and(|h| {
                            interp.env.get(h).is_some() || interp.funcs.contains_key(h)
                        });
                        if !head_shadowed {
                            let head = head.unwrap_or_default();
                            let canonical = if interp.enums.contains_key(&head) {
                                Some(head.clone())
                            } else {
                                interp
                                    .import_aliases
                                    .get(&head)
                                    .filter(|q| interp.enums.contains_key(q.as_str()))
                                    .cloned()
                            };
                            if let Some(ename) = canonical {
                                if interp
                                    .resolve_path_value(&parts[..parts.len() - 1], pspan)
                                    .is_err()
                                {
                                    let variant = parts.last().cloned().unwrap_or_default();
                                    let has = interp.enums.get(&ename).and_then(|vs| {
                                        vs.iter().find(|(v, _)| v == &variant).map(|(_, h)| *h)
                                    });
                                    match has {
                                        Some(has_payload) if has_payload == (args.len() == 1) => {
                                            let fields = if has_payload {
                                                vec![(
                                                    std::sync::Arc::from("value"),
                                                    args.into_iter().next().unwrap(),
                                                )]
                                            } else {
                                                Vec::new()
                                            };
                                            self.stack.push(Value::Object(std::sync::Arc::new(
                                                crate::value::ObjectValue {
                                                    name: format!("{ename}.{variant}"),
                                                    fields,
                                                },
                                            )));
                                            continue;
                                        }
                                        // Wrong arity or unknown variant:
                                        // fall through to the method path
                                        // so the error names the real cause.
                                        _ => {}
                                    }
                                }
                            }
                        }
                        // Method on an inline unit variant (`E.V.m()`):
                        // same resolution as above, then method dispatch
                        // on the constructed receiver. Payload variants
                        // can't chain (checker rejects with bind-first).
                        // `head2` is built once and reused below (it was
                        // joined three times per call before).
                        let head2: Option<String> = if parts.len() >= 3 {
                            Some(parts[..parts.len() - 2].join("."))
                        } else {
                            None
                        };
                        let head2_shadowed = head2.as_ref().is_some_and(|h| {
                            interp.env.get(h).is_some() || interp.funcs.contains_key(h)
                        });
                        if !head2_shadowed {
                            let head2 = head2.unwrap_or_default();
                            let canonical2 = if interp.enums.contains_key(&head2) {
                                Some(head2.clone())
                            } else {
                                interp
                                    .import_aliases
                                    .get(&head2)
                                    .filter(|q| interp.enums.contains_key(q.as_str()))
                                    .cloned()
                            };
                            if let Some(ename) = canonical2 {
                                let variant = parts[parts.len() - 2].clone();
                                let method = parts.last().cloned().unwrap_or_default();
                                let is_unit = interp
                                    .enums
                                    .get(&ename)
                                    .and_then(|vs| {
                                        vs.iter().find(|(v, _)| v == &variant).map(|(_, h)| *h)
                                    })
                                    .is_some_and(|has| !has);
                                if is_unit {
                                    let recv = Value::Object(std::sync::Arc::new(
                                        crate::value::ObjectValue {
                                            name: format!("{ename}.{variant}"),
                                            fields: Vec::new(),
                                        },
                                    ));
                                    if let Ok((f, recv)) =
                                        interp.lookup_method_recv(&recv, &method, span)
                                    {
                                        // Reuse the scratch buffer: prepend
                                        // the receiver instead of allocating
                                        // a second Vec (shift is O(argc),
                                        // argc is tiny on this path).
                                        let mut arg_vals = args;
                                        arg_vals.insert(0, recv);
                                        self.frames.last_mut().unwrap().ip = ip;
                                        self.call_value(f, arg_vals, span, interp)?;
                                        re_cache!();
                                        yield_check!();
                                        continue;
                                    }
                                }
                            }
                        }
                        if interp.resolve_path_value(parts, pspan).is_err() {
                            let method = parts.last().unwrap();
                            let recv =
                                interp.resolve_path_value(&parts[..parts.len() - 1], pspan)?;
                            // sqlz fast path (CallPath form): Db handle +
                            // query/exec/close dispatches straight to the
                            // canonical sqlz.* native only.
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
                                    });
                                match entry {
                                    Some(e) => {
                                        let mut arg_vals = args;
                                        arg_vals.insert(0, recv);
                                        self.frames.last_mut().unwrap().ip = ip;
                                        let result = (e.f)(interp, &mut arg_vals, span)?;
                                        self.stack.push(result);
                                        return_scratch_args(arg_vals);
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
                            let (f, recv) = interp.lookup_method_recv(&recv, method, pspan)?;
                            let mut arg_vals = args;
                            arg_vals.insert(0, recv);
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
                    let mut args = take_scratch_args();
                    // +1: method fallbacks prepend the receiver into this
                    // same buffer (`insert(0, recv)`) instead of allocating
                    // a second Vec.
                    args.reserve(argc as usize + 1);
                    for _ in 0..argc {
                        args.push(self.stack.pop().unwrap());
                    }
                    if args.len() > 1 {
                        args.reverse();
                    }
                    let recv = self.stack.pop().unwrap();
                    self.frames.last_mut().unwrap().ip = ip;
                    // sqlz fast path: `mydb.query/exec` on a Db handle
                    // bypasses object_field (Db has no struct fields) and
                    // dispatches straight to the canonical sqlz.* native.
                    if matches!(recv, Value::Db(_))
                        && matches!(name.as_str(), "query" | "exec" | "close")
                    {
                        let native_name = format!("sqlz.{name}");
                        let entry =
                            interp.natives.get(&native_name).copied().or_else(|| {
                                interp.natives.get(&format!("std.sqlz.{name}")).copied()
                            });
                        match entry {
                            Some(e) => {
                                let mut arg_vals = args;
                                arg_vals.insert(0, recv);
                                let result = (e.f)(interp, &mut arg_vals, span)?;
                                self.stack.push(result);
                                return_scratch_args(arg_vals);
                                re_cache!();
                                yield_check!();
                                continue;
                            }
                            None => {
                                return Err(self.error(format!("undefined method `{name}`"), span));
                            }
                        }
                    }
                    // Probe fields without building the miss error: a
                    // method call is a field miss by definition, so the
                    // formatted error was pure waste on every call.
                    match object_field_opt(&recv, name) {
                        Some(f) => self.call_value(f, args, span, interp)?,
                        None => {
                            let (f, recv) = interp.lookup_method_recv(&recv, name, span)?;
                            let mut arg_vals = args;
                            arg_vals.insert(0, recv);
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
                        // Display semantics (see `Value::to_display_string`):
                        // interpolation unwraps Option; `dbg`/`:?` keeps it.
                        out.push_str(&p.to_display_string());
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
                    let parent = interp.env.parent_link().expect("ExitScope at top level");
                    interp.env = parent;
                }
                Op::PopN { n, span } => {
                    let len = self.stack.len();
                    // Checked (never wrapping): a short stack is a compiler
                    // imbalance — report it with location instead of
                    // panicking (debug) or corrupting silently (release).
                    let drop = (*n as usize).saturating_add(1);
                    if len < drop {
                        return Err(self.error(
                            format!("block cleanup drops {drop} values from a {len}-deep stack"),
                            *span,
                        ));
                    }
                    let result = self.stack.pop().unwrap();
                    self.stack.truncate(len - drop);
                    self.stack.push(result);
                }
                Op::DeferRecord => {
                    let closure = self.stack.pop().unwrap();
                    self.defer_stack.push(closure);
                }
                Op::CallNative { name, argc, span } => {
                    let argc = *argc;
                    let span = *span;
                    // `len` is the most frequent native call in string and
                    // collection code and a pure function of one stack value,
                    // so specialise it ahead of the dynamic path: no args
                    // buffer, no registry lookup, no frame re-cache. The
                    // fixed dispatch cost measured ~150ns per call, against
                    // ~10ns here. The semantics are the registry entry's own
                    // — `value_len`/`len_error` are the shared implementation
                    // (they already serve `LenField`) and the registered
                    // arity is exactly 1, so the generic arity gate cannot
                    // disagree for this spelling; every other arity and every
                    // other native stays on the generic path below.
                    if argc == 1 && name.as_str() == "len" {
                        let v = self.stack.pop().unwrap();
                        if let Some(n) = Self::value_len(&v) {
                            self.stack.push(Value::Int(n));
                        } else {
                            return Err(Self::len_error(&v, span));
                        }
                        // A pending cooperative yield must resume *after*
                        // this op, never re-execute it: the argument has been
                        // consumed and the result pushed, so syncing `ip`
                        // (as the generic path does before it calls the
                        // native) is what makes re-entry idempotent.
                        self.frames.last_mut().unwrap().ip = ip;
                        yield_check!();
                        continue;
                    }
                    // Scratch args buffer: reuses one allocation per thread
                    // instead of allocating an args `Vec` per call (~50ns
                    // saved on the hot path). Reentrant calls (a native
                    // calling back into the VM) find it busy and fall back
                    // to a fresh `Vec` — always correct, just slower.
                    // A panic unwinding past here leaks the buffer (bugs
                    // only); the next call allocates anew.
                    let mut args = take_scratch_args();
                    for _ in 0..argc {
                        args.push(self.stack.pop().unwrap());
                    }
                    args.reverse();
                    // Arity gate (mirrors `Interp::call`): variadic db
                    // query/exec (+ aliases) skip it; every other native
                    // must see exactly its registered arity. Without this
                    // the direct-dispatch fast path silently accepts
                    // wrong-arity calls that `CallPath` rejects.
                    let is_db = matches!(
                        name.as_str(),
                        "sqlz.query"
                            | "std.sqlz.query"
                            | "sqlz.exec"
                            | "std.sqlz.exec"
                            | "pg.query"
                            | "pg.exec"
                            | "postgres.query"
                            | "postgres.exec"
                            | "std.sqlz.postgres.query"
                            | "std.sqlz.postgres.exec"
                            | "my.query"
                            | "my.exec"
                            | "mysql.query"
                            | "mysql.exec"
                            | "std.sqlz.mysql.query"
                            | "std.sqlz.mysql.exec"
                    );
                    // sqlz.query/sqlz.exec carry a variable
                    // number of bound params; resolve the entry without an
                    // arity gate (Interp::call skips it for sqlz.* too).
                    // (`pg.*`/`my.*` only exist as import aliases of the
                    // nested `std.sqlz.postgres/mysql`.)
                    let entry = zz_checker::lookup_std(&interp.natives, name).copied();
                    let arity = match &entry {
                        Some(e) => e.arity,
                        // C-only plugins resolve by name from the registry.
                        None => match crate::c_abi::arity_of(name) {
                            Some(a) => a,
                            None => {
                                return Err(self.error(format!("unknown native `{name}`"), span));
                            }
                        },
                    };
                    if !is_db
                        && args.len() != arity
                        && !fill_default_headers(name, &mut args, arity)
                    {
                        return_scratch_args(args);
                        return Err(self.error(
                            format!("expected {} arguments, found {}", arity, argc),
                            span,
                        ));
                    }
                    self.frames.last_mut().unwrap().ip = ip;
                    let result = match entry {
                        Some(e) => (e.f)(interp, &mut args, span)?,
                        None => match crate::c_abi::call(name, &mut args, span) {
                            Some(r) => r?,
                            None => {
                                return Err(self.error(format!("unknown native `{name}`"), span));
                            }
                        },
                    };
                    self.stack.push(result);
                    return_scratch_args(args);
                    re_cache!();
                    yield_check!();
                }
            }
        }
    }

    /// Enter a bytecode function's frame: move the arguments onto the
    /// stack, switch the active environment, and push the frame.
    ///
    /// Shared by [`Vm::call_value`] and the direct-call fast path so frame
    /// setup is identical wherever a chunk is entered.
    fn enter_call_frame(
        &mut self,
        chunk: Arc<Chunk>,
        env: EnvLink,
        args: Vec<Value>,
        span: Span,
        interp: &mut Interp,
    ) {
        let stack_base = self.stack.len();
        let mut args = args;
        // Append (not extend+drain): elements move onto the stack while
        // the buffer keeps its allocation and goes back to the scratch
        // slot below — steady-state calls never allocate.
        self.stack.append(&mut args);
        return_scratch_args(args);
        self.open_frame(stack_base, chunk, env, span, interp);
    }

    /// Push a call frame whose arguments already occupy the top of the
    /// value stack (`stack_base = stack.len() - argc`).
    ///
    /// The direct-call site memo resolves the callee *before* touching the
    /// stack, so a hit copies nothing: the arguments are already in
    /// parameter order where the callee expects them.
    fn open_frame(
        &mut self,
        stack_base: usize,
        chunk: Arc<Chunk>,
        env: EnvLink,
        span: Span,
        interp: &mut Interp,
    ) {
        let prev_env = std::mem::replace(&mut interp.env, env);
        let saved_defers = std::mem::take(&mut self.defer_stack);
        self.frames.push(Frame {
            chunk,
            ip: 0,
            prev_env,
            stack_base,
            defer_stack: saved_defers,
            func_span: span,
        });
    }

    /// Call the compiled function bound under `name` without materialising
    /// a `Value`.
    ///
    /// `resolve_path_value` answers a callable name with a cloned
    /// `Value::Func`, which copies the function's parameters and defaults
    /// on **every** call. Whole-program functions are registered in the
    /// environment *and* the function table, so recursive code (`fib`)
    /// pays that twice per call — once for the `is_direct` probe's
    /// discarded value and once for the callee.
    ///
    /// Returns the arguments back when the call needs the general path:
    /// a shadowing non-function binding, a native, a canonical `std.*`
    /// alias, an arity that needs defaults filled, or a closure with no
    /// bytecode. Everything this handles is exactly what the general path
    /// would have done, without the copies.
    pub(super) fn try_enter_direct_call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        span: Span,
        interp: &mut Interp,
        site: Option<(u64, usize, u64)>,
    ) -> Option<Vec<Value>> {
        match self.resolve_direct(name, args.len(), interp) {
            Some((chunk, env)) => {
                if let Some((chunk_id, ip, gen)) = site {
                    self.fill_direct_call(chunk_id, ip, args.len() as u16, gen, &chunk, &env);
                }
                self.enter_call_frame(chunk, env, args, span, interp);
                None
            }
            None => Some(args),
        }
    }

    /// Resolve a call name to a compiled function the direct path can enter
    /// without materialising a `Value`.
    ///
    /// `None` means "take the general path": a shadowing non-function
    /// binding, a native, a canonical `std.*` alias, defaults to fill, a
    /// chunk-less closure, or an arity mismatch.
    fn resolve_direct(
        &self,
        name: &str,
        argc: usize,
        interp: &Interp,
    ) -> Option<(Arc<Chunk>, crate::env::EnvLink)> {
        // Bindings win over the function table (mirrors
        // `resolve_path_value`'s environment-first order).
        match interp.env.bound(name) {
            Some(Bound::Compiled { chunk, env, params }) if argc == params => {
                return Some((chunk, env));
            }
            Some(_) => return None,
            None => {}
        }
        // Unbound: a direct hit in the function table is the same value
        // the general path resolves next. Canonical `std.*` aliases are
        // left to it (rare, and they carry no local binding to shadow).
        let fv = interp.funcs.get(name)?;
        if argc != fv.params.len() {
            return None;
        }
        let chunk = fv.chunk.as_ref()?;
        Some((Arc::clone(chunk), fv.env.clone()))
    }

    /// Memoized [`Vm::resolve_direct`] for a `CallPath` site.
    fn direct_call_hit(
        &self,
        chunk_id: u64,
        ip: usize,
        argc: u16,
        gen: u64,
    ) -> Option<(Arc<Chunk>, crate::env::EnvLink)> {
        let e = self.call_cache[(chunk_id as usize ^ ip) & (CALL_CACHE_LEN - 1)].as_ref()?;
        if e.chunk_id == chunk_id && e.ip == ip && e.argc == argc && e.gen == gen {
            Some((Arc::clone(&e.callee), e.env.clone()))
        } else {
            None
        }
    }

    fn fill_direct_call(
        &mut self,
        chunk_id: u64,
        ip: usize,
        argc: u16,
        gen: u64,
        callee: &Arc<Chunk>,
        env: &crate::env::EnvLink,
    ) {
        let i = (chunk_id as usize ^ ip) & (CALL_CACHE_LEN - 1);
        self.call_cache[i] = Some(CachedDirect {
            chunk_id,
            ip,
            argc,
            gen,
            callee: Arc::clone(callee),
            env: env.clone(),
        });
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
                let mut args = args;
                if args.len() > fv.params.len() {
                    let expected = fv.params.len();
                    let found = args.len();
                    return_scratch_args(args);
                    return Err(self.error(
                        format!("expected {expected} arguments, found {found}"),
                        span,
                    ));
                }
                if args.len() < fv.params.len() {
                    // Fill omitted trailing defaults (evaluated against the
                    // caller's environment). Mirrors `Interp::call_func` so
                    // cross-module calls — whose defaults the per-program
                    // compiler never saw — still honor the checker contract.
                    let missing = fv.params.len() - args.len();
                    let start = args.len();
                    for i in 0..missing {
                        match fv.params[start + i].default.as_ref() {
                            Some(d) => {
                                let v = interp.eval(d)?.into_value()?;
                                args.push(v);
                            }
                            None => {
                                // `.zzc` loads carry pre-compiled default
                                // bodies instead of AST (no AST is
                                // available on the load path). The chunk
                                // runs against the caller's environment —
                                // exactly where `interp.eval` above runs.
                                if let Some(Some(dchunk)) = fv.chunk_defaults.get(start + i) {
                                    let mut sub = Vm::new();
                                    match sub.run_chunk(dchunk, interp)? {
                                        Flow::Value(v) | Flow::Return(v) => args.push(v),
                                        Flow::Break(span) | Flow::Continue(span) => {
                                            return Err(
                                                self.error("invalid default-argument body", span)
                                            );
                                        }
                                        Flow::Yield(_) => {
                                            return Err(self.error(
                                                "invalid default-argument body",
                                                Span::new(0, 0),
                                            ));
                                        }
                                    }
                                } else {
                                    let expected = fv.params.len();
                                    return_scratch_args(args);
                                    return Err(self.error(
                                        format!("expected {expected} arguments, found {start}"),
                                        span,
                                    ));
                                }
                            }
                        }
                    }
                }
                let chunk = fv.chunk.clone().expect("chunk presence checked above");
                self.enter_call_frame(chunk, fv.env.clone(), args, span, interp);
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
        // Keep the return payload in a register (plain `Value` + bool) instead
        // of wrapping it in `Option<Value>` — avoids a tag check on every
        // return and keeps the value live across `truncate`.
        let mut payload = Value::Unit;
        let mut has_payload = false;
        let mut brk: Option<Span> = None;
        let mut ctn: Option<Span> = None;
        match flow {
            Flow::Return(v) => {
                payload = v;
                has_payload = true;
            }
            Flow::Break(s) => brk = Some(s),
            Flow::Continue(s) => ctn = Some(s),
            Flow::Value(_) => {
                return Unwind::Error(self.error(
                    "internal error: unwind_frame on a plain value",
                    Span::default(),
                ))
            }
            Flow::Yield(_) => return Unwind::Error(crate::runtime::EvalError::yield_escape()),
        };
        let f = self.frames.pop().unwrap();
        self.loops.retain(|li| li.frame_idx < self.frames.len());
        self.stack.truncate(f.stack_base);
        interp.env = f.prev_env;
        if self.frames.is_empty() {
            if let Some(span) = brk {
                return Unwind::Escaped(Flow::Break(span));
            }
            if let Some(span) = ctn {
                return Unwind::Escaped(Flow::Continue(span));
            }
            return Unwind::Escaped(Flow::Return(if has_payload {
                payload
            } else {
                Value::Unit
            }));
        }
        if let Some(span) = brk {
            return Unwind::Error(self.error("`break` outside of a loop", span));
        }
        if let Some(span) = ctn {
            return Unwind::Error(self.error("`continue` outside of a loop", span));
        }
        self.stack
            .push(if has_payload { payload } else { Value::Unit });
        Unwind::Continue
    }

    /// Build a backtrace string from the current call stack.
    pub(crate) fn backtrace(&self) -> Vec<(String, Span)> {
        self.frames
            .iter()
            .map(|f| (String::new(), f.func_span))
            .collect()
    }

    /// Create an EvalError with the current backtrace attached.
    fn error(&self, message: impl Into<String>, span: Span) -> EvalError {
        EvalError::new(message, span).with_backtrace(self.backtrace())
    }

    /// `LoadVar` resolution chain (env, then funcs, then natives): shared by
    /// `LoadVar` and the `TakeVar` fallback for non-env bindings.
    fn load_var(interp: &mut Interp, name: &str, span: Span) -> Result<Value, EvalError> {
        interp
            .env
            .get(name)
            .or_else(|| {
                zz_checker::lookup_std(&interp.funcs, name)
                    .map(|fv| Value::Func(std::rc::Rc::clone(fv)))
            })
            .or_else(|| {
                zz_checker::lookup_std(&interp.natives, name).map(|entry| {
                    Value::Native(Box::new(NativeFunc {
                        name: name.to_string(),
                        arity: entry.arity,
                    }))
                })
            })
            // C-only plugins (direct dlsym, no Rust shim).
            .or_else(|| crate::c_abi::native_value(name))
            .or_else(|| {
                // Selective-import alias (miss-only): resolve
                // `ns.sym` like a qualified path. See
                // tree-walker Ident eval for the full note.
                let qualified = interp.import_aliases.get(name).cloned();
                qualified.and_then(|q| {
                    let parts: Vec<String> = q.split('.').map(str::to_string).collect();
                    interp.resolve_path_value(&parts, span).ok()
                })
            })
            .ok_or_else(|| EvalError::new(format!("undefined variable `{name}`"), span))
    }

    /// `TakeVar` fallback for names that are not env bindings (funcs,
    /// natives, plugins): no take is possible, so load a clone exactly
    /// like `LoadVar`.
    fn load_var_fallback(interp: &mut Interp, name: &str, span: Span) -> Result<Value, EvalError> {
        Self::load_var(interp, name, span)
    }

    /// Length of a value with `len` semantics (mirrors the `len`
    /// native: arrays, tuples, byte buffers, strings by char count,
    /// dicts, and ranges). `None` for unsized values.
    fn value_len(v: &Value) -> Option<i64> {
        match v {
            Value::Array(vs) => Some(vs.len() as i64),
            Value::Tuple(vs) => Some(vs.len() as i64),
            Value::Bytes(b) => Some(b.len() as i64),
            Value::Str(st) => Some(if st.is_ascii() {
                st.len()
            } else {
                st.chars().count()
            } as i64),
            Value::Dict(entries) => Some(entries.len() as i64),
            Value::Range(r) => {
                let (start, stop, step) = (r.start, r.end, r.step);
                if step == 0 {
                    return None;
                }
                Some(
                    if (step > 0 && start < stop) || (step < 0 && start > stop) {
                        (stop - start + step - if step > 0 { 1 } else { -1 }) / step
                    } else {
                        0
                    },
                )
            }
            _ => None,
        }
    }

    /// Exact `len` failure for unsized values (mirrors the native,
    /// including the zero-step range message).
    fn len_error(v: &Value, span: Span) -> EvalError {
        if matches!(v, Value::Range(r) if r.step == 0) {
            return EvalError::new("range step cannot be zero", span);
        }
        EvalError::new(
            format!("len expects array, tuple, bytes, string, dict, or range, found `{v}`"),
            span,
        )
    }

    /// Take the value out of a fused-op home (leaving `Unit`). Returns
    /// `None` for unbound env homes (the caller errors like `LoadVar`).
    fn take_home(vm: &mut Vm, interp: &mut Interp, home: &TakeHome) -> Option<Value> {
        match home {
            TakeHome::Slot(slot) => {
                let base = vm.frames.last().unwrap().stack_base;
                Some(std::mem::replace(
                    &mut vm.stack[base + *slot as usize],
                    Value::Unit,
                ))
            }
            TakeHome::Env(name) => interp.env.take(name),
        }
    }

    /// Store a value back into a fused-op home.
    fn restore_home(vm: &mut Vm, interp: &mut Interp, home: &TakeHome, v: Value) {
        match home {
            TakeHome::Slot(slot) => {
                let base = vm.frames.last().unwrap().stack_base;
                vm.stack[base + *slot as usize] = v;
            }
            TakeHome::Env(name) => {
                // The take proved the binding; nothing unbinds in the
                // window, so this always hits the same owning scope with
                // zero clones. The define arm is unreachable insurance.
                if let Err(v) = interp.env.try_assign(name, v) {
                    interp.env.define(name, v);
                }
            }
        }
    }

    /// Push `elem` into the named field of an owned `home` object in place:
    /// takes the field out (leaving `Unit`), reuses the owned `Vec`, stores
    /// the field back. Direct fields answer inline; promoted, missing, or
    /// non-object fields take the generic path (clone read + push +
    /// promotion-aware write — same value, one extra clone).
    /// Errors carry the message; the caller attaches its span and restores
    /// the home first (REPL error-path parity).
    fn field_push_take(
        home: &mut Value,
        field: &str,
        elem: Value,
        span: Span,
    ) -> Result<(), String> {
        if let Value::Object(o) = home {
            let o = std::sync::Arc::make_mut(o);
            if let Some((_, slot)) = o.fields.iter_mut().find(|(n, _)| n.as_ref() == field) {
                let mut vs = match std::mem::replace(slot, Value::Unit) {
                    Value::Array(b) => {
                        std::sync::Arc::try_unwrap(b).unwrap_or_else(|a| (*a).clone())
                    }
                    other => {
                        *slot = other;
                        return Err(format!("`vec.push` expects an array, found `{}`", slot));
                    }
                };
                vs.push(elem);
                *slot = Value::Array(std::sync::Arc::new(vs));
                return Ok(());
            }
        }
        // Generic path: mirrors `GetField` + `vec.push` + `SetField`.
        let cur = match object_field(home, field, span) {
            Ok(v) => v,
            Err(e) => {
                return Err(e.message);
            }
        };
        let mut vs = match cur {
            Value::Array(b) => std::sync::Arc::try_unwrap(b).unwrap_or_else(|a| (*a).clone()),
            other => {
                return Err(format!("`vec.push` expects an array, found `{other}`"));
            }
        };
        vs.push(elem);
        let pushed = Value::Array(std::sync::Arc::new(vs));
        match set_object_field(home, field, pushed, span) {
            Ok(()) => Ok(()),
            Err(e) => Err(e.message),
        }
    }
}
