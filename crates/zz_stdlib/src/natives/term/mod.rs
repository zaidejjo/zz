//! `std.term` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::term` (which also backs the AOT `zz_term_*` FFI).
//! Fallible ops surface as `Result<_, str>` so piped/CI stdin degrades
//! gracefully (`.err("std.term.<op>: not a tty")`); `is_tty` is total.

use zz_runtime::{EvalError, Interp, Span, Value};

fn ok_wrap(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

fn err_wrap(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

pub(crate) fn term_enable_raw(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match zz_native_rt::term::enable_raw() {
        Ok(()) => Ok(ok_wrap(Value::Unit)),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

pub(crate) fn term_disable_raw(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match zz_native_rt::term::disable_raw() {
        Ok(()) => Ok(ok_wrap(Value::Unit)),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

pub(crate) fn term_read_key(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match zz_native_rt::term::read_key() {
        Ok(b) => Ok(ok_wrap(Value::Int(b))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

pub(crate) fn term_get_size(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match zz_native_rt::term::get_size() {
        Ok((cols, rows)) => Ok(ok_wrap(Value::Array(Box::new(vec![
            Value::Int(cols),
            Value::Int(rows),
        ])))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

pub(crate) fn term_is_tty(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Bool(zz_native_rt::term::is_tty()))
}
