use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn result_unwrap(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let res = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for result.unwrap", span))?;
    match res {
        Value::Result(Ok(v)) => Ok(*v),
        Value::Result(Err(e)) => Err(EvalError::new(
            format!("result.unwrap: called unwrap on .err({e})"),
            span,
        )),
        other => Err(EvalError::new(
            format!("result.unwrap: expected a result, found `{other}`"),
            span,
        )),
    }
}

pub(crate) fn result_unwrap_or(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let res = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for result.unwrap_or", span))?;
    let default = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("missing `default` argument for result.unwrap_or", span))?;
    match res {
        Value::Result(Ok(v)) => Ok(*v),
        Value::Result(Err(_)) => Ok(default),
        other => Err(EvalError::new(
            format!("result.unwrap_or: expected a result, found `{other}`"),
            span,
        )),
    }
}

pub(crate) fn result_expect(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let res = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for result.expect", span))?;
    let msg = expect_str(args, 1, "result.expect", span)?;
    match res {
        Value::Result(Ok(v)) => Ok(*v),
        Value::Result(Err(e)) => Err(EvalError::new(format!("result.expect: {msg}: {e}"), span)),
        other => Err(EvalError::new(
            format!("result.expect: expected a result, found `{other}`"),
            span,
        )),
    }
}
