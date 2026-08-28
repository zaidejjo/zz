use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Value};

pub(crate) fn option_unwrap(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    let opt = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for option.unwrap",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    match opt {
        Value::Option(Some(v)) => Ok(*v),
        Value::Option(None) => Err(EvalError::new(
            "called unwrap on .none",
            zz_runtime::Span::new(0, 0),
        )),
        other => Err(EvalError::new(
            format!("option.unwrap: expected an option, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn option_unwrap_or(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    let opt = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for option.unwrap_or",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    let default = args.get(1).cloned().ok_or_else(|| {
        EvalError::new(
            "missing `default` argument for option.unwrap_or",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    match opt {
        Value::Option(Some(v)) => Ok(*v),
        Value::Option(None) => Ok(default),
        other => Err(EvalError::new(
            format!("option.unwrap_or: expected an option, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn option_expect(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    let opt = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for option.expect",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    let msg = expect_str(args, 1, "option.expect")?;
    match opt {
        Value::Option(Some(v)) => Ok(*v),
        Value::Option(None) => Err(EvalError::new(
            format!("option.expect: {msg}"),
            zz_runtime::Span::new(0, 0),
        )),
        other => Err(EvalError::new(
            format!("option.expect: expected an option, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}
