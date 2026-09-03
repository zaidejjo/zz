use crate::natives::{arg, expect_str};
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn str_length(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.length")?;
    Ok(Value::Int(s.chars().count() as i64))
}

pub(crate) fn str_split(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.split")?;
    let sep = expect_str(args, 1, "std.str.split")?;
    let parts: Vec<Value> = s
        .split(&sep)
        .map(|p| Value::Str(p.to_string().into()))
        .collect();
    Ok(Value::Array(Box::new(parts)))
}

pub(crate) fn str_contains(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.contains")?;
    let sub = expect_str(args, 1, "std.str.contains")?;
    Ok(Value::Bool(s.contains(&sub)))
}

pub(crate) fn str_trim(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.trim")?;
    Ok(Value::Str(s.trim().to_string().into()))
}

pub(crate) fn str_to_upper(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.to_upper")?;
    Ok(Value::Str(s.to_uppercase().into()))
}

pub(crate) fn str_to_lower(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.to_lower")?;
    Ok(Value::Str(s.to_lowercase().into()))
}

pub(crate) fn str_replace(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.replace")?;
    let old = expect_str(args, 1, "str.replace")?;
    let new = expect_str(args, 2, "str.replace")?;
    Ok(Value::Str(s.replace(&old, &new).into()))
}

pub(crate) fn str_starts_with(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.starts_with")?;
    let prefix = expect_str(args, 1, "str.starts_with")?;
    Ok(Value::Bool(s.starts_with(&prefix)))
}

pub(crate) fn str_ends_with(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.ends_with")?;
    let suffix = expect_str(args, 1, "str.ends_with")?;
    Ok(Value::Bool(s.ends_with(&suffix)))
}

pub(crate) fn str_join(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let items = match arg(args, 0, "str.join")?.clone() {
        Value::Array(arr) => arr,
        other => {
            return Err(EvalError::new(
                format!("`str.join` expects an array, found `{other}`"),
                _span,
            ));
        }
    };
    let sep = expect_str(args, 1, "str.join")?;
    let strs: Vec<String> = items
        .iter()
        .map(|v| match v {
            Value::Str(s) => (**s).clone(),
            other => other.to_string().into(),
        })
        .collect();
    Ok(Value::Str(strs.join(&sep).into()))
}

pub(crate) fn str_trim_start(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.trim_start")?;
    Ok(Value::Str(s.trim_start().to_string().into()))
}

pub(crate) fn str_trim_end(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.trim_end")?;
    Ok(Value::Str(s.trim_end().to_string().into()))
}
