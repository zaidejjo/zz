use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn fs_read_file(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.read_file")?;
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(Value::Result(Box::new(Ok(Value::Str(contents.into()))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("{e}").into(),
        ))))),
    }
}

pub(crate) fn fs_write_file(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.write_file")?;
    let contents = expect_str(args, 1, "std.fs.write_file")?;
    match std::fs::write(&path, contents) {
        Ok(()) => Ok(Value::Result(Box::new(Ok(Value::Unit)))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("{e}").into(),
        ))))),
    }
}

pub(crate) fn fs_exists(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.exists")?;
    Ok(Value::Bool(std::path::Path::new(&path).exists()))
}

pub(crate) fn fs_remove_file(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.remove_file")?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(Value::Result(Box::new(Ok(Value::Unit)))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("{e}").into(),
        ))))),
    }
}
