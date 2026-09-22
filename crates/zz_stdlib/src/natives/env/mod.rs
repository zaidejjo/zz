use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Value};

pub(crate) fn env_get_var(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.env.get_var")?;
    match std::env::var(&name) {
        Ok(v) => Ok(Value::Option(Some(Box::new(Value::Str(v))))),
        Err(_) => Ok(Value::Option(None)),
    }
}

pub(crate) fn env_args(interp: &mut Interp, _args: &mut Vec<Value>) -> Result<Value, EvalError> {
    Ok(Value::Array(
        interp.args.iter().map(|s| Value::Str(s.clone())).collect(),
    ))
}
