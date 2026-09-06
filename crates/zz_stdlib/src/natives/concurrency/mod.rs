//! Channel concurrency primitives for ZZ.
//!
//! Provides `chan()`, `chan.send()`, `chan.recv()`, `chan.try_recv()`,
//! `spawn()`, and `task.join()`.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use zz_runtime::{EvalError, Interp, Span, Value};

use zz_runtime::value::snapshot_env;
use zz_runtime::value::{ChanInner, ChanState, TaskJoinState};

/// `chan()` — create a new unbounded channel.
pub(crate) fn chan_new(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    #[allow(clippy::arc_with_non_send_sync)]
    let state = Arc::new(ChanState {
        inner: Mutex::new(ChanInner {
            queue: VecDeque::new(),
        }),
        cvar: Condvar::new(),
    });
    Ok(Value::Chan(state))
}

/// `chan.send(ch, v)` — push a value into the channel.
pub(crate) fn chan_send(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let ch = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `ch` for chan.send", span))?;
    let v = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `v` for chan.send", span))?;

    match ch {
        Value::Chan(state) => {
            let mut guard = state
                .inner
                .lock()
                .map_err(|e| EvalError::new(format!("chan.send: lock poisoned: {e}"), span))?;
            guard.queue.push_back(v);
            state.cvar.notify_one();
            Ok(Value::Unit)
        }
        other => Err(EvalError::new(
            format!("chan.send: expected a channel, found `{other}`"),
            span,
        )),
    }
}

/// `chan.recv(ch)` — blocking receive. Waits until a value is available.
pub(crate) fn chan_recv(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let ch = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `ch` for chan.recv", span))?;

    match ch {
        Value::Chan(state) => {
            let guard = state
                .inner
                .lock()
                .map_err(|e| EvalError::new(format!("chan.recv: lock poisoned: {e}"), span))?;
            let mut guard = state
                .cvar
                .wait_while(guard, |inner| inner.queue.is_empty())
                .map_err(|e| {
                    EvalError::new(format!("chan.recv: condvar wait failed: {e}"), span)
                })?;
            Ok(guard.queue.pop_front().expect("queue non-empty after wait"))
        }
        other => Err(EvalError::new(
            format!("chan.recv: expected a channel, found `{other}`"),
            span,
        )),
    }
}

/// `chan.try_recv(ch)` — non-blocking receive. Returns `.some(v)` or `.none`.
pub(crate) fn chan_try_recv(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let ch = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `ch` for chan.try_recv", span))?;

    match ch {
        Value::Chan(state) => {
            let mut guard = state
                .inner
                .lock()
                .map_err(|e| EvalError::new(format!("chan.try_recv: lock poisoned: {e}"), span))?;
            Ok(match guard.queue.pop_front() {
                Some(v) => Value::Option(Some(Box::new(v))),
                None => Value::Option(None),
            })
        }
        other => Err(EvalError::new(
            format!("chan.try_recv: expected a channel, found `{other}`"),
            span,
        )),
    }
}

// ── TaskJoin methods ────────────────────────────────────────────────────────

/// `spawn(closure)` — spawn a closure on a new OS thread.
///
/// The closure's captured environment is deep-cloned (snapshot) so the
/// spawned thread gets its own independent copy of all captured variables.
/// Returns a `TaskJoin` handle whose `.recv()` method blocks until the
/// task completes and yields the closure's return value.
pub(crate) fn spawn(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fv = match args.first() {
        Some(Value::Func(f)) => (**f).clone(),
        Some(other) => {
            return Err(EvalError::new(
                format!("spawn: expected a closure, found `{other}`"),
                span,
            ))
        }
        None => {
            return Err(EvalError::new(
                "spawn: expected a closure, found nothing",
                span,
            ))
        }
    };

    let chunk = fv.chunk.ok_or_else(|| {
        EvalError::new(
            "spawn: closure has no compiled chunk (tree-walker closures cannot be spawned)",
            span,
        )
    })?;

    // 1. Snapshot captured env → self-contained flat map.
    let snapshot = snapshot_env(&fv.env);

    // 2. Clone read-only globals.
    let natives = interp.natives.clone();
    let structs = interp.structs.clone();

    // 3. Prepare shared result slot.
    let result = Arc::new(Mutex::new(None));
    let handle_for_thread = Arc::clone(&result);
    let handle_for_join = Arc::clone(&result);

    // 4. Spawn OS thread.
    std::thread::spawn(move || {
        let mut new_interp = Interp::with_natives(natives);
        new_interp.structs = structs;

        // Seed env from snapshot.
        {
            let mut env = new_interp.env.borrow_mut();
            for (name, val) in snapshot {
                env.define(&name, val);
            }
        }

        let mut vm = zz_runtime::vm::Vm::new();
        let outcome = match vm.run_chunk(&chunk, &mut new_interp) {
            Ok(zz_runtime::runtime::Flow::Value(v)) => Ok(v),
            Ok(_) => Ok(Value::Unit),
            Err(e) => Err(e.message),
        };
        *handle_for_thread.lock().unwrap() = Some(outcome);
    });

    #[allow(clippy::arc_with_non_send_sync)]
    Ok(Value::TaskJoin(Arc::new(TaskJoinState {
        result: handle_for_join,
        cvar: Condvar::new(),
    })))
}

/// `task.join(handle)` — blocking receive on a task join handle.
/// Returns the task's result, or `.err(msg)` if the task panicked.
pub(crate) fn task_join(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `handle` for task.join", span))?;

    match handle {
        Value::TaskJoin(state) => {
            let guard = state
                .result
                .lock()
                .map_err(|e| EvalError::new(format!("task.join: lock poisoned: {e}"), span))?;
            let mut guard = state
                .cvar
                .wait_while(guard, |inner| inner.is_none())
                .map_err(|e| {
                    EvalError::new(format!("task.join: condvar wait failed: {e}"), span)
                })?;
            match guard.take().expect("result set after wait") {
                Ok(v) => Ok(v),
                Err(msg) => Ok(Value::Result(Box::new(Err(Value::Str(Box::new(msg)))))),
            }
        }
        other => Err(EvalError::new(
            format!("task.join: expected a task join handle, found `{other}`"),
            span,
        )),
    }
}

/// `task.try_join(handle)` — non-blocking check on a task join handle.
/// Returns `.some(result)` or `.none`.
#[allow(dead_code)]
pub(crate) fn task_try_join(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `handle` for task.try_join", span))?;

    match handle {
        Value::TaskJoin(state) => {
            let mut guard = state
                .result
                .lock()
                .map_err(|e| EvalError::new(format!("task.try_join: lock poisoned: {e}"), span))?;
            match guard.take() {
                Some(Ok(v)) => Ok(Value::Option(Some(Box::new(Value::Result(Box::new(Ok(
                    v,
                ))))))),
                Some(Err(msg)) => Ok(Value::Option(Some(Box::new(Value::Result(Box::new(Err(
                    Value::Str(Box::new(msg)),
                ))))))),
                None => Ok(Value::Option(None)),
            }
        }
        other => Err(EvalError::new(
            format!("task.try_join: expected a task join handle, found `{other}`"),
            span,
        )),
    }
}
