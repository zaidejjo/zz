use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Value};

pub(crate) fn result_unwrap(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    let res = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for result.unwrap",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    match res {
        Value::Result(Ok(v)) => Ok(*v),
        Value::Result(Err(e)) => Err(EvalError::new(
            format!("result.unwrap: called unwrap on .err({e})"),
            zz_runtime::Span::new(0, 0),
        )),
        other => Err(EvalError::new(
            format!("result.unwrap: expected a result, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn result_unwrap_or(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    let res = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for result.unwrap_or",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    let default = args.get(1).cloned().ok_or_else(|| {
        EvalError::new(
            "missing `default` argument for result.unwrap_or",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    match res {
        Value::Result(Ok(v)) => Ok(*v),
        Value::Result(Err(_)) => Ok(default),
        other => Err(EvalError::new(
            format!("result.unwrap_or: expected a result, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn result_expect(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    let res = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for result.expect",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    let msg = expect_str(args, 1, "result.expect")?;
    match res {
        Value::Result(Ok(v)) => Ok(*v),
        Value::Result(Err(e)) => Err(EvalError::new(
            format!("result.expect: {msg}: {e}"),
            zz_runtime::Span::new(0, 0),
        )),
        other => Err(EvalError::new(
            format!("result.expect: expected a result, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}
