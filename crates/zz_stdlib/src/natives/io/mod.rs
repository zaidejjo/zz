use std::io::Write;

use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn printz(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for std.io.printz", _span))?;
    print!("{v}");
    Ok(Value::Unit)
}

pub(crate) fn println(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for std.io.println", _span))?;
    println!("{v}");
    Ok(Value::Unit)
}

pub(crate) fn read_line(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    // Optional prompt argument
    if !args.is_empty() {
        let prompt = expect_str(args, 0, "input", span)?;
        print!("{prompt}");
        std::io::stdout()
            .flush()
            .map_err(|e| EvalError::new(format!("failed to flush stdout: {e}"), span))?;
    }
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| EvalError::new(format!("failed to read line: {e}"), span))?;
    // Strip the trailing newline (and CR for Windows line endings).
    Ok(Value::Str(line.trim_end_matches(['\n', '\r']).to_string()))
}
