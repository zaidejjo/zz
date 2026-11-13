//! `std.signal` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::signal` (which also backs the AOT `zz_signal_*` FFI).
//!
//! Delivery model (yield-point, not async preemption):
//! - `signal.install(signum, handler)` stores `handler` (a `Func`) in a
//!   signum-keyed registry and installs a `sigaction` via the native RT.
//!   The signal handler itself only flips an atomic flag — it never touches
//!   ZZ state, so it is async-signal-safe.
//! - `signal.pending(signum)` checks+clears the flag.
//! - `signal.run_pending()` is the delivery point: at a yield point (or
//!   explicitly in the user's event loop) it invokes every registered
//!   handler whose flag is set, through the interpreter. Handlers run once
//!   per pending flag (the flag is consumed by `pending`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use zz_runtime::{EvalError, Interp, Span, Value};

/// `signum -> handler closure`. `Func` values carry an `Rc` env, so they are
/// `Send` but not `Sync`; the mutex confines them to the installing thread,
/// which is fine because delivery runs on that same thread at yield points.
static HANDLERS: OnceLock<Mutex<HashMap<i32, Value>>> = OnceLock::new();

fn handlers() -> &'static Mutex<HashMap<i32, Value>> {
    HANDLERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn ok_wrap(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

fn err_wrap(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

/// `signal.install(signum: int, handler: Func<[], unit>) -> Result<unit, str>`.
pub(crate) fn signal_install(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let signum = match args.first() {
        Some(Value::Int(n)) => *n as i32,
        other => {
            return Err(EvalError::new(
                format!(
                    "std.signal.install: expects `int` signum at arg 0, found `{}`",
                    other
                        .map(|v| v.type_name())
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                span,
            ))
        }
    };
    let handler = match args.get(1) {
        Some(Value::Func(_)) => args.remove(1),
        other => {
            return Err(EvalError::new(
                format!(
                    "std.signal.install: expects a closure handler at arg 1, found `{}`",
                    other
                        .map(|v| v.type_name())
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                span,
            ))
        }
    };
    if !(1..64).contains(&signum) {
        return Err(EvalError::new(
            format!("std.signal.install: invalid signum {signum}"),
            span,
        ));
    }
    // Install the OS-level handler (sets atomic flag + self-pipe).
    match zz_native_rt::signal::signal_install(signum) {
        Ok(()) => {}
        Err(msg) => return Ok(err_wrap(msg)),
    }
    handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(signum, handler);
    Ok(ok_wrap(Value::Unit))
}

/// `signal.pending(signum: int) -> bool`.
pub(crate) fn signal_pending(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let signum = match args.first() {
        Some(Value::Int(n)) => *n as i32,
        other => {
            return Err(EvalError::new(
                format!(
                    "std.signal.pending: expects `int` signum at arg 0, found `{}`",
                    other
                        .map(|v| v.type_name())
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                span,
            ))
        }
    };
    Ok(Value::Bool(zz_native_rt::signal::signal_pending(signum)))
}

/// `signal.raise(signum: int) -> Result<unit, str>`.
pub(crate) fn signal_raise(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let signum = match args.first() {
        Some(Value::Int(n)) => *n as i32,
        other => {
            return Err(EvalError::new(
                format!(
                    "std.signal.raise: expects `int` signum at arg 0, found `{}`",
                    other
                        .map(|v| v.type_name())
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                span,
            ))
        }
    };
    match zz_native_rt::signal::signal_raise(signum) {
        Ok(()) => Ok(ok_wrap(Value::Unit)),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `signal.run_pending() -> Result<unit, str>`.
///
/// Delivery point: invoke every registered handler whose pending flag is
/// set. Called by the user's event loop at yield points. Handlers are
/// removed after one delivery (re-install to re-arm).
pub(crate) fn signal_run_pending(
    interp: &mut Interp,
    _args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let guard = handlers().lock().unwrap_or_else(|e| e.into_inner());
    let pending: Vec<i32> = guard
        .keys()
        .copied()
        .filter(|&s| zz_native_rt::signal::signal_pending(s))
        .collect();
    drop(guard);

    for signum in pending {
        let handler = {
            let mut g = handlers().lock().unwrap_or_else(|e| e.into_inner());
            g.remove(&signum)
        };
        let Some(Value::Func(f)) = handler else {
            continue;
        };
        // Invoke the handler through the interpreter at a yield point.
        // Errors are surfaced but do not abort other pending deliveries.
        let _ = interp.call(Value::Func(f), vec![], span);
    }
    Ok(ok_wrap(Value::Unit))
}
