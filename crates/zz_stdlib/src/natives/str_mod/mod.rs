use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn str_length(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.length", span)?;
    Ok(Value::Int(s.chars().count() as i64))
}

pub(crate) fn str_split(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.split", span)?;
    let sep = expect_str(args, 1, "std.str.split", span)?;
    let parts: Vec<Value> = s.split(&sep).map(|p| Value::Str(p.to_string())).collect();
    Ok(Value::Array(parts))
}

pub(crate) fn str_contains(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.contains", span)?;
    let sub = expect_str(args, 1, "std.str.contains", span)?;
    Ok(Value::Bool(s.contains(&sub)))
}

pub(crate) fn str_trim(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.trim", span)?;
    Ok(Value::Str(s.trim().to_string()))
}

pub(crate) fn str_to_upper(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.to_upper", span)?;
    Ok(Value::Str(s.to_uppercase()))
}

pub(crate) fn str_to_lower(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.to_lower", span)?;
    Ok(Value::Str(s.to_lowercase()))
}

pub(crate) fn str_replace(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.replace", span)?;
    let old = expect_str(args, 1, "str.replace", span)?;
    let new = expect_str(args, 2, "str.replace", span)?;
    Ok(Value::Str(s.replace(&old, &new)))
}

pub(crate) fn str_starts_with(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.starts_with", span)?;
    let prefix = expect_str(args, 1, "str.starts_with", span)?;
    Ok(Value::Bool(s.starts_with(&prefix)))
}

pub(crate) fn str_ends_with(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.ends_with", span)?;
    let suffix = expect_str(args, 1, "str.ends_with", span)?;
    Ok(Value::Bool(s.ends_with(&suffix)))
}
