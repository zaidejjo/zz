//! `std.pty` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::pty` (which also backs the AOT `zz_pty_*` FFI). Fallible
//! ops surface as `Result<_, str>` so non-tty fds degrade gracefully;
//! `isatty` is total.

use zz_runtime::{EvalError, Interp, Span, Value};

fn ok_wrap(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

fn err_wrap(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

fn expect_fd(args: &mut Vec<Value>, i: usize, name: &str, span: Span) -> Result<i32, EvalError> {
    match args.get(i) {
        Some(Value::Int(n)) => Ok(*n as i32),
        other => Err(EvalError::new(
            format!(
                "{name}: expects `int` fd at arg {i}, found `{}`",
                other
                    .map(|v| v.type_name())
                    .unwrap_or_else(|| "nothing".to_string())
            ),
            span,
        )),
    }
}

/// `pty.open() -> Result<[int, str], str>` — `[master_fd, slave_path]`.
pub(crate) fn pty_open(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match zz_native_rt::pty::pty_open() {
        Ok((master, slave)) => Ok(ok_wrap(Value::Array(std::sync::Arc::new(vec![
            Value::Int(master as i64),
            Value::Int(slave as i64),
        ])))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `pty.read(fd, n) -> Result<str, str>` (lossy UTF-8).
pub(crate) fn pty_read(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fd = expect_fd(args, 0, "std.pty.read", span)?;
    let n = match args.get(1) {
        Some(Value::Int(n)) => *n as usize,
        other => {
            return Err(EvalError::new(
                format!(
                    "std.pty.read: expects `int` n at arg 1, found `{}`",
                    other
                        .map(|v| v.type_name())
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                span,
            ))
        }
    };
    match zz_native_rt::pty::pty_read(fd, n) {
        Ok(data) => Ok(ok_wrap(Value::Str(data.into()))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `pty.write(fd, data) -> Result<int, str>` (bytes written).
pub(crate) fn pty_write(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fd = expect_fd(args, 0, "std.pty.write", span)?;
    let data = match args.get(1) {
        Some(Value::Str(s)) => s.as_bytes().to_vec(),
        other => {
            return Err(EvalError::new(
                format!(
                    "std.pty.write: expects `str` data at arg 1, found `{}`",
                    other
                        .map(|v| v.type_name())
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                span,
            ))
        }
    };
    match zz_native_rt::pty::pty_write(fd, &data) {
        Ok(count) => Ok(ok_wrap(Value::Int(count))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `pty.ptsname(fd) -> Result<str, str>`.
pub(crate) fn pty_ptsname(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fd = expect_fd(args, 0, "std.pty.ptsname", span)?;
    match zz_native_rt::pty::pty_ptsname(fd) {
        Ok(path) => Ok(ok_wrap(Value::Str(path.into()))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `pty.grantpt(fd) -> Result<unit, str>`.
pub(crate) fn pty_grantpt(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fd = expect_fd(args, 0, "std.pty.grantpt", span)?;
    match zz_native_rt::pty::pty_grantpt(fd) {
        Ok(()) => Ok(ok_wrap(Value::Unit)),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `pty.unlockpt(fd) -> Result<unit, str>`.
pub(crate) fn pty_unlockpt(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fd = expect_fd(args, 0, "std.pty.unlockpt", span)?;
    match zz_native_rt::pty::pty_unlockpt(fd) {
        Ok(()) => Ok(ok_wrap(Value::Unit)),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `pty.set_winsz(fd, cols, rows) -> Result<unit, str>`.
pub(crate) fn pty_set_winsz(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fd = expect_fd(args, 0, "std.pty.set_winsz", span)?;
    let cols = expect_int_arg(args, 1, "std.pty.set_winsz", span)?;
    let rows = expect_int_arg(args, 2, "std.pty.set_winsz", span)?;
    match zz_native_rt::pty::pty_set_winsz(fd, cols, rows) {
        Ok(()) => Ok(ok_wrap(Value::Unit)),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `pty.isatty(fd) -> bool`.
pub(crate) fn pty_isatty(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fd = expect_fd(args, 0, "std.pty.isatty", span)?;
    Ok(Value::Bool(zz_native_rt::pty::pty_isatty(fd)))
}

fn expect_int_arg(args: &[Value], i: usize, name: &str, span: Span) -> Result<i32, EvalError> {
    match args.get(i) {
        Some(Value::Int(n)) => Ok(*n as i32),
        other => Err(EvalError::new(
            format!(
                "{name}: expects `int` at arg {i}, found `{}`",
                other
                    .map(|v| v.type_name())
                    .unwrap_or_else(|| "nothing".to_string())
            ),
            span,
        )),
    }
}
