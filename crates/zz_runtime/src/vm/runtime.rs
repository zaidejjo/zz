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
use crate::value::{FuncValue, NativeFunc, Value};

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
        });

        loop {
            let (code, constants, ip) = {
                let f = self.frames.last().unwrap();
                let chunk = unsafe { &*Rc::as_ptr(&f.chunk) };
                (&chunk.code, &chunk.constants, f.ip)
            };

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
                    continue;
                }

                self.defer_stack = parent_defers;

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
                    let v = eval_binary(*op, l, r, *span)?;
                    self.stack.push(v);
                }
                Op::UnOp(op, span) => {
                    let v = self.stack.pop().unwrap();
                    let v = eval_unary(*op, v, *span)?;
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
                    let defers: Vec<Value> = self.defer_stack.drain(..).collect();
                    if defers.is_empty() {
                        match self.unwind_frame(Flow::Return(v), interp) {
                            Unwind::Continue => {}
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
                    self.stack.push(Value::Unit);
                }
                Op::ForNext { var, exit, in_env } => {
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
                        other => Err(EvalError::new(
                            format!("slice bound must be `int`, found `{other}`"),
                            *span,
                        )),
                    };
                    let v = slice_value(&ov, bound(s)?, bound(e)?, *span)?;
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
                            self.stack.push(Value::Bool(true));
                            self.stack.push(*inner);
                        }
                        Value::Option(None) => {
                            self.stack.push(Value::Bool(false));
                            self.stack.push(Value::Unit);
                        }
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
                    match object_field(&recv, name, span) {
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
                    let formatted = crate::runtime::format::format_value_with_spec(&val, &spec_str);
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
                    return Err(EvalError::new(
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
