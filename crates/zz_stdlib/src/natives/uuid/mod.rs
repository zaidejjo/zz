//! `std.uuid` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::uuid` (which also backs the AOT `zz_uuid_*` FFI).
//! UUIDs cross as canonical strings; no handles are involved.

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::expect_str;

pub(crate) fn uuid_v4(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Str(zz_native_rt::uuid::v4().into()))
}

pub(crate) fn uuid_v7(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Str(zz_native_rt::uuid::v7().into()))
}

pub(crate) fn uuid_parse(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.uuid.parse")?;
    match zz_native_rt::uuid::parse(&s) {
        Ok(u) => Ok(Value::Result(Box::new(Ok(Value::Str(u.into()))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(e.into()))))),
    }
}

pub(crate) fn uuid_is_valid(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.uuid.is_valid")?;
    Ok(Value::Bool(zz_native_rt::uuid::is_valid(&s)))
}
