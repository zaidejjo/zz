use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn env_get_var(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.env.get_var")?;
    match std::env::var(&name) {
        Ok(v) => Ok(Value::Option(Some(Box::new(Value::Str(v))))),
        Err(_) => Ok(Value::Option(None)),
    }
}

pub(crate) fn env_var(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.env.var")?;
    match std::env::var(&name) {
        Ok(v) => Ok(Value::Result(Ok(Box::new(Value::Str(v))))),
        Err(_) => Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "environment variable `{name}` not set"
        )))))),
    }
}

pub(crate) fn env_args(
    interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Array(
        interp.args.iter().map(|s| Value::Str(s.clone())).collect(),
    ))
}
