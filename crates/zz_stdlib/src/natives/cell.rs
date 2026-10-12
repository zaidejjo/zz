//! `cell.*` natives (VM side).
//!
//! Shared-mutable slots: `cell.new(v)` stores a clone in the
//! `zz_native_rt` pool under tag `"cell"` and returns an opaque handle;
//! `get` clones back out, `set` replaces, `drop` releases. Each op takes
//! the pool lock once, so single ops are atomic across green threads;
//! read-modify-write sequences are NOT atomic (coordinate with `chan`
//! when that matters).

use std::sync::{Arc, Mutex};

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::arg;

fn expect_cell(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
    span: Span,
) -> Result<zz_native_rt::Handle, EvalError> {
    match arg(args, i, name)? {
        Value::Opaque(h) if h.tag == "cell" => Ok((**h).clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a cell, found `{other}`"),
            span,
        )),
    }
}

/// `cell.new(v: T) -> cell`
pub(crate) fn cell_new(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = arg(args, 0, "std.cell.new")?.clone();
    let h = zz_native_rt::alloc("cell", Arc::new(Mutex::new(v)));
    let _ = span;
    Ok(Value::Opaque(Box::new(h)))
}

/// `cell.get(c: cell) -> Result<T, str>` (`.err` when dropped).
pub(crate) fn cell_get(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_cell(args, 0, "std.cell.get", span)?;
    match zz_native_rt::payload::<Mutex<Value>>(h.id, "cell") {
        Some(m) => {
            let guard = m.lock().unwrap_or_else(|p| p.into_inner());
            Ok(Value::Result(Box::new(Ok(guard.clone()))))
        }
        None => Ok(Value::Result(Box::new(Err(Value::Str(
            "cell.get: cell was dropped".to_string().into(),
        ))))),
    }
}

/// `cell.set(c: cell, v: T) -> Result<bool, str>` (`.err` when dropped).
pub(crate) fn cell_set(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_cell(args, 0, "std.cell.set", span)?;
    let v = arg(args, 1, "std.cell.set")?.clone();
    match zz_native_rt::payload::<Mutex<Value>>(h.id, "cell") {
        Some(m) => {
            let mut guard = m.lock().unwrap_or_else(|p| p.into_inner());
            *guard = v;
            Ok(Value::Result(Box::new(Ok(Value::Bool(true)))))
        }
        None => Ok(Value::Result(Box::new(Err(Value::Str(
            "cell.set: cell was dropped".to_string().into(),
        ))))),
    }
}

/// `cell.drop(c: cell) -> bool` (idempotent).
pub(crate) fn cell_drop(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_cell(args, 0, "std.cell.drop", span)?;
    Ok(Value::Bool(zz_native_rt::drop_handle(h.id)))
}
