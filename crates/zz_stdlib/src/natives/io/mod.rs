use std::io::Write;

use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

/// Unwrap consecutive `Result::Ok` layers for stdout presentation.
///
/// `println(.ok(v))` prints `v` directly instead of `.ok(v)`, so standard
/// execution output stays clean. Interpolation (`"{v}"`), `str(v)`, and
/// `Debug` keep the full `.ok(...)` representation.
fn for_stdout(mut v: Value, span: Span) -> Result<Value, EvalError> {
    loop {
        match v {
            Value::Result(r) => match *r {
                Ok(inner) => v = inner,
                Err(e) => return Err(err_to_throw(&e.to_string(), span)),
            },
            other => return Ok(other),
        }
    }
}

/// Turn a printed `.err(payload)` into a readable, hinted error.
///
/// Structured `fs:<op>:<code>: <detail>` diagnostics become sentences like
/// `cannot read 'text.txt': no such file or directory` plus a `hint:` note;
/// anything else surfaces verbatim with a generic recovery hint. The CLI
/// renders the returned `EvalError` with span context and ANSI colors.
fn err_to_throw(payload: &str, span: Span) -> EvalError {
    let mut parts = payload.splitn(4, ':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("fs"), Some(op), Some(code), Some(detail)) => {
            let detail = detail.trim();
            let verb = match op.trim() {
                "read" | "read_bytes" => "read",
                "write" => "write to",
                "append" => "append to",
                "copy" => "copy",
                "move" | "rename" => "move",
                "remove" | "remove_file" => "remove",
                "mkdir" | "mkdir_all" => "create directory",
                "read_dir" | "readdir" => "list directory",
                "remove_dir_all" => "remove directory",
                "walk_dir" => "walk directory",
                "stat" => "stat",
                "open" => "open",
                "read_chunk" => "read from file",
                "write_chunk" => "write to file",
                "seek" => "seek in file",
                "flush" => "flush file",
                "close" => "close file",
                _ => op.trim(),
            };
            let reason = match code.trim() {
                "not_found" => "no such file or directory",
                "permission_denied" => "permission denied",
                "already_exists" => "file already exists",
                "invalid_input" => "invalid argument",
                "not_empty" => "directory is not empty",
                "closed" => "file handle is closed",
                _ => "input/output error",
            };
            let hint = match code.trim() {
                "not_found" => {
                    "check that the path is correct — relative paths resolve \
                     from the current working directory"
                }
                "permission_denied" => "check read/write permissions for the current user",
                "already_exists" => "remove the existing file first, or pick another path",
                "invalid_input" => {
                    "check the arguments (e.g. File.open mode must be one of r, w, a)"
                }
                "not_empty" => "remove the contents first, or use fs.remove_dir_all",
                "closed" => "the handle was already closed — open it again with File.open",
                _ => "check disk health and available space",
            };
            let mut e = EvalError::new(format!("cannot {verb} '{detail}': {reason}"), span);
            e.notes.push(format!("hint: {hint}"));
            e
        }
        _ => {
            let mut e = EvalError::new(format!("error value printed: {payload}"), span);
            e.notes.push(
                "hint: handle .err explicitly with match to recover instead of aborting"
                    .to_string(),
            );
            e
        }
    }
}

pub(crate) fn print(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for print", span))?;
    // A printed `.err` throws (readable + hinted); see `for_stdout`.
    print!("{}", for_stdout(v, span)?);
    Ok(Value::Unit)
}

pub(crate) fn println(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for println", span))?;
    // A printed `.err` throws (readable + hinted); see `for_stdout`.
    println!("{}", for_stdout(v, span)?);
    Ok(Value::Unit)
}

pub(crate) fn read_line(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    // Optional prompt argument
    if !args.is_empty() {
        let prompt = expect_str(args, 0, "input")?;
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
    Ok(Value::Str(
        line.trim_end_matches(['\n', '\r']).to_string().into(),
    ))
}
