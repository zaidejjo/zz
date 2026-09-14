//! `std.log` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::log` (which also backs the AOT `zz_log_*` FFI).
//! Records go to stderr (never stdout); spans are [`Value::Opaque`]
//! handles under tag `"span"`, so `sp.end()` method syntax works through
//! the standard tag-based dispatch.

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::expect_str;

pub(crate) fn log_set_level(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.log.set_level")?;
    Ok(Value::Bool(zz_native_rt::log::set_level(&name)))
}

pub(crate) fn log_get_level(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Str(Box::new(
        zz_native_rt::log::get_level().to_string(),
    )))
}

pub(crate) fn log_set_format(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.log.set_format")?;
    Ok(Value::Bool(zz_native_rt::log::set_format(&name)))
}

pub(crate) fn log_to_file(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.log.to_file")?;
    Ok(Value::Bool(zz_native_rt::log::to_file(&path)))
}

pub(crate) fn log_to_stderr(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    zz_native_rt::log::to_stderr();
    Ok(Value::Unit)
}

macro_rules! level_native {
    ($name:ident, $level:ident) => {
        pub(crate) fn $name(
            _interp: &mut Interp,
            args: &mut Vec<Value>,
            _span: Span,
        ) -> Result<Value, EvalError> {
            let msg = expect_str(args, 0, concat!("std.log.", stringify!($level)))?;
            zz_native_rt::log::log_at(zz_native_rt::log::$level, &msg);
            Ok(Value::Unit)
        }
    };
}

level_native!(log_trace, TRACE);
level_native!(log_debug, DEBUG);
level_native!(log_info, INFO);
level_native!(log_warn, WARN);
level_native!(log_error, ERROR);

pub(crate) fn log_span_begin(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.log.span_begin")?;
    Ok(Value::Opaque(Box::new(zz_native_rt::log::span_begin(
        &name,
    ))))
}

pub(crate) fn span_end(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let id = match crate::natives::arg(args, 0, "span.end")? {
        Value::Opaque(h) if h.tag == zz_native_rt::log::SPAN_TAG => h.id,
        other => {
            return Err(EvalError::new(
                format!("`span.end` expects a span handle, found `{other}`"),
                span,
            ));
        }
    };
    match zz_native_rt::log::span_end(id) {
        Some(us) => Ok(Value::Int(us)),
        None => Err(EvalError::new(
            "`span.end`: span handle is no longer valid".to_string(),
            span,
        )),
    }
}
