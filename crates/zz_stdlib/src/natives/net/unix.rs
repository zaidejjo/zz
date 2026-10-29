//! `net.unix_*` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared driver in
//! `zz_native_rt::unix_conn` (which also backs the AOT `zz_unix_*` FFI).
//! Connections are [`Value::Opaque`] handles under tag `"unix"`, listeners
//! under `"unix_listener"`, so method syntax works through tag dispatch.

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::{arg, expect_int, expect_str};

fn expect_unix(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
    span: Span,
) -> Result<zz_native_rt::Handle, EvalError> {
    match arg(args, i, name)? {
        Value::Opaque(h) if h.tag == zz_native_rt::unix_conn::TAG => Ok((**h).clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a unix connection, found `{other}`"),
            span,
        )),
    }
}

fn expect_unix_listener(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
    span: Span,
) -> Result<u64, EvalError> {
    match arg(args, i, name)? {
        Value::Opaque(h) if h.tag == zz_native_rt::unix_conn::LISTENER_TAG => Ok(h.id),
        other => Err(EvalError::new(
            format!("`{name}` expects a unix listener, found `{other}`"),
            span,
        )),
    }
}

fn gone(name: &str, span: Span) -> EvalError {
    EvalError::new(
        format!("`{name}`: unix connection is no longer valid"),
        span,
    )
}

fn ok_wrap(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

fn err_wrap(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

/// `net.unix_connect(path, timeout_ms) -> Result<unix, str>`
pub(crate) fn unix_connect(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.net.unix_connect")?;
    let ms = expect_int(args, 1, "std.net.unix_connect")?.max(0) as u64;
    match zz_native_rt::unix_conn::connect(&path, ms) {
        Ok(h) => Ok(ok_wrap(Value::Opaque(Box::new(h)))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `net.unix_listen(path) -> Result<unix_listener, str>`
pub(crate) fn unix_listen(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.net.unix_listen")?;
    match zz_native_rt::unix_conn::listen(&path) {
        Ok(h) => Ok(ok_wrap(Value::Opaque(Box::new(h)))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `net.unix_accept(listener) -> Result<unix, str>`
pub(crate) fn unix_accept(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let id = expect_unix_listener(args, 0, "std.net.unix_accept", span)?;
    match zz_native_rt::unix_conn::accept(id) {
        Ok(h) => Ok(ok_wrap(Value::Opaque(Box::new(h)))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

/// `unix_listener.close(listener)`.
pub(crate) fn unix_listener_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let id = expect_unix_listener(args, 0, "std.net.unix_listener_close", span)?;
    zz_native_rt::drop_handle(id);
    Ok(ok_wrap(Value::Bool(true)))
}

pub(crate) fn unix_write(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_write", span)?;
    let data = expect_str(args, 1, "std.net.unix_write")?;
    match zz_native_rt::unix_conn::with(h.id, |c| c.write_all(data.as_bytes())) {
        Some(Ok(())) => Ok(ok_wrap(Value::Int(data.len() as i64))),
        Some(Err(e)) => Ok(err_wrap(format!("unix:io: write failed ({e})"))),
        None => Err(gone("std.net.unix_write", span)),
    }
}

pub(crate) fn unix_write_bytes(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_write_bytes", span)?;
    let data = match arg(args, 1, "std.net.unix_write_bytes")? {
        Value::Bytes(b) => b.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.unix_write_bytes: expected bytes, found `{other}`"),
                span,
            ))
        }
    };
    let len = data.len();
    let bytes = data.as_slice().to_vec();
    match zz_native_rt::unix_conn::with(h.id, |c| c.write_all(&bytes)) {
        Some(Ok(())) => Ok(ok_wrap(Value::Int(len as i64))),
        Some(Err(e)) => Ok(err_wrap(format!("unix:io: write failed ({e})"))),
        None => Err(gone("std.net.unix_write_bytes", span)),
    }
}

pub(crate) fn unix_read(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_read", span)?;
    let max = expect_int(args, 1, "std.net.unix_read")?.clamp(1, 8 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; max];
    match zz_native_rt::unix_conn::with(h.id, |c| c.read(&mut buf)) {
        Some(Ok(0)) => Ok(err_wrap("unix:closed: connection closed".to_string())),
        Some(Ok(n)) => {
            buf.truncate(n);
            Ok(ok_wrap(Value::Str(
                String::from_utf8_lossy(&buf).into_owned().into(),
            )))
        }
        Some(Err(e)) => Ok(err_wrap(format!("unix:io: read failed ({e})"))),
        None => Err(gone("std.net.unix_read", span)),
    }
}

pub(crate) fn unix_read_bytes(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_read_bytes", span)?;
    let max = expect_int(args, 1, "std.net.unix_read_bytes")?.clamp(1, 8 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; max];
    match zz_native_rt::unix_conn::with(h.id, |c| c.read(&mut buf)) {
        Some(Ok(0)) => Ok(err_wrap("unix:closed: connection closed".to_string())),
        Some(Ok(n)) => {
            buf.truncate(n);
            Ok(ok_wrap(Value::Bytes(Box::new(
                zz_runtime::BytesData::from_vec(buf),
            ))))
        }
        Some(Err(e)) => Ok(err_wrap(format!("unix:io: read failed ({e})"))),
        None => Err(gone("std.net.unix_read_bytes", span)),
    }
}

pub(crate) fn unix_read_exact(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_read_exact", span)?;
    let n = expect_int(args, 1, "std.net.unix_read_exact")?;
    if n <= 0 || n > 64 * 1024 * 1024 {
        return Ok(err_wrap(format!(
            "unix:io: bad read length {n} (want 1..67108864)"
        )));
    }
    let mut out = vec![0u8; n as usize];
    match zz_native_rt::unix_conn::with(h.id, |c| c.read_exact(&mut out)) {
        Some(Ok(())) => Ok(ok_wrap(Value::Bytes(Box::new(
            zz_runtime::BytesData::from_vec(out),
        )))),
        Some(Err(e)) => Ok(err_wrap(format!("unix:io: read_exact failed ({e})"))),
        None => Err(gone("std.net.unix_read_exact", span)),
    }
}

pub(crate) fn unix_readline(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_readline", span)?;
    let mut line: Vec<u8> = Vec::new();
    let mut one = [0u8; 1];
    loop {
        let r = zz_native_rt::unix_conn::with(h.id, |c| c.read_exact(&mut one));
        match r {
            Some(Ok(())) => {
                if one[0] == b'\n' {
                    break;
                }
                line.push(one[0]);
                if line.len() > 64 * 1024 {
                    return Ok(err_wrap("unix:io: line too long (max 64 KiB)".to_string()));
                }
            }
            Some(Err(e)) => {
                if line.is_empty() {
                    return Ok(err_wrap(format!("unix:io: readline failed ({e})")));
                }
                break;
            }
            None => return Err(gone("std.net.unix_readline", span)),
        }
    }
    if !line.is_empty() && line[line.len() - 1] == b'\r' {
        line.pop();
    }
    Ok(ok_wrap(Value::Str(
        String::from_utf8_lossy(&line).into_owned().into(),
    )))
}

pub(crate) fn unix_set_read_timeout(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_set_read_timeout", span)?;
    let ms = expect_int(args, 1, "std.net.unix_set_read_timeout")?;
    let dur = if ms <= 0 {
        None
    } else {
        Some(std::time::Duration::from_millis(ms as u64))
    };
    match zz_native_rt::unix_conn::with(h.id, |c| c.set_read_timeout(dur)) {
        Some(Ok(())) => Ok(ok_wrap(Value::Bool(true))),
        Some(Err(e)) => Ok(err_wrap(format!("unix:io: set_read_timeout failed ({e})"))),
        None => Err(gone("std.net.unix_set_read_timeout", span)),
    }
}

pub(crate) fn unix_set_write_timeout(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_set_write_timeout", span)?;
    let ms = expect_int(args, 1, "std.net.unix_set_write_timeout")?;
    let dur = if ms <= 0 {
        None
    } else {
        Some(std::time::Duration::from_millis(ms as u64))
    };
    match zz_native_rt::unix_conn::with(h.id, |c| c.set_write_timeout(dur)) {
        Some(Ok(())) => Ok(ok_wrap(Value::Bool(true))),
        Some(Err(e)) => Ok(err_wrap(format!("unix:io: set_write_timeout failed ({e})"))),
        None => Err(gone("std.net.unix_set_write_timeout", span)),
    }
}

pub(crate) fn unix_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_unix(args, 0, "std.net.unix_close", span)?;
    zz_native_rt::unix_conn::with(h.id, |c| {
        let _ = c.stream.shutdown(std::net::Shutdown::Both);
    });
    zz_native_rt::drop_handle(h.id);
    Ok(ok_wrap(Value::Unit))
}
