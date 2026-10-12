//! `net.tls_*` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared driver in
//! `zz_native_rt::tls_conn` (which also backs the AOT `zz_tls_*` FFI).
//! Connections are [`Value::Opaque`] handles under tag `"tls"`, so
//! `c.write(..)` / `c.read_line()?` method syntax works through the
//! standard tag-based dispatch.

use std::io::{Read, Write};

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::{arg, expect_int, expect_str};

fn expect_tls(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
    span: Span,
) -> Result<zz_native_rt::Handle, EvalError> {
    match arg(args, i, name)? {
        Value::Opaque(h) if h.tag == zz_native_rt::tls_conn::TAG => Ok((**h).clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a tls connection, found `{other}`"),
            span,
        )),
    }
}

fn gone(name: &str, span: Span) -> EvalError {
    EvalError::new(format!("`{name}`: tls connection is no longer valid"), span)
}

fn ok_wrap(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

fn err_wrap(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

fn tls_err(e: &str) -> Value {
    err_wrap(e.to_string())
}

/// `net.tls_connect(addr, server_name, timeout_ms) -> Result<tls, str>`
pub(crate) fn tls_connect(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let addr = expect_str(args, 0, "std.net.tls_connect")?;
    let name = expect_str(args, 1, "std.net.tls_connect")?;
    let ms = expect_int(args, 2, "std.net.tls_connect")?.max(0) as u64;
    match zz_native_rt::tls_conn::connect(&addr, &name, ms, None, false) {
        Ok(h) => Ok(ok_wrap(Value::Opaque(Box::new(h)))),
        Err(msg) => Ok(tls_err(&msg)),
    }
}

/// `net.tls_connect_with_ca(addr, server_name, ca_file, timeout_ms)`
pub(crate) fn tls_connect_with_ca(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let addr = expect_str(args, 0, "std.net.tls_connect_with_ca")?;
    let name = expect_str(args, 1, "std.net.tls_connect_with_ca")?;
    let ca = expect_str(args, 2, "std.net.tls_connect_with_ca")?;
    let ms = expect_int(args, 3, "std.net.tls_connect_with_ca")?.max(0) as u64;
    match zz_native_rt::tls_conn::connect(&addr, &name, ms, Some(ca.as_str()), false) {
        Ok(h) => Ok(ok_wrap(Value::Opaque(Box::new(h)))),
        Err(msg) => Ok(tls_err(&msg)),
    }
}

/// `net.tls_connect_insecure(addr, server_name, timeout_ms)` — verification OFF.
pub(crate) fn tls_connect_insecure(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let addr = expect_str(args, 0, "std.net.tls_connect_insecure")?;
    let name = expect_str(args, 1, "std.net.tls_connect_insecure")?;
    let ms = expect_int(args, 2, "std.net.tls_connect_insecure")?.max(0) as u64;
    match zz_native_rt::tls_conn::connect(&addr, &name, ms, None, true) {
        Ok(h) => Ok(ok_wrap(Value::Opaque(Box::new(h)))),
        Err(msg) => Ok(tls_err(&msg)),
    }
}

pub(crate) fn tls_write(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_write", span)?;
    let data = expect_str(args, 1, "std.net.tls_write")?;
    match zz_native_rt::tls_conn::with(h.id, |c| c.stream.write_all(data.as_bytes())) {
        Some(Ok(())) => Ok(ok_wrap(Value::Int(data.len() as i64))),
        Some(Err(e)) => Ok(tls_err(&format!("tls:io: write failed ({e})"))),
        None => Err(gone("std.net.tls_write", span)),
    }
}

pub(crate) fn tls_write_bytes(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_write_bytes", span)?;
    let data = match arg(args, 1, "std.net.tls_write_bytes")? {
        Value::Bytes(b) => b.clone(),
        other => {
            return Err(EvalError::new(
                "std.net.tls_write_bytes: expected bytes".to_string()
                    + &format!(", found `{other}`"),
                span,
            ))
        }
    };
    let len = data.len();
    let bytes = data.as_slice().to_vec();
    match zz_native_rt::tls_conn::with(h.id, |c| c.stream.write_all(&bytes)) {
        Some(Ok(())) => Ok(ok_wrap(Value::Int(len as i64))),
        Some(Err(e)) => Ok(tls_err(&format!("tls:io: write failed ({e})"))),
        None => Err(gone("std.net.tls_write_bytes", span)),
    }
}

pub(crate) fn tls_read(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_read", span)?;
    let max = expect_int(args, 1, "std.net.tls_read")?.clamp(1, 8 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; max];
    match zz_native_rt::tls_conn::with(h.id, |c| c.stream.read(&mut buf)) {
        Some(Ok(0)) => Ok(tls_err("tls:io: connection closed")),
        Some(Ok(n)) => {
            buf.truncate(n);
            Ok(ok_wrap(Value::Str(
                String::from_utf8_lossy(&buf).into_owned().into(),
            )))
        }
        Some(Err(e)) => Ok(tls_err(&format!("tls:io: read failed ({e})"))),
        None => Err(gone("std.net.tls_read", span)),
    }
}

pub(crate) fn tls_read_bytes(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_read_bytes", span)?;
    let max = expect_int(args, 1, "std.net.tls_read_bytes")?.clamp(1, 8 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; max];
    match zz_native_rt::tls_conn::with(h.id, |c| c.stream.read(&mut buf)) {
        Some(Ok(0)) => Ok(tls_err("tls:io: connection closed")),
        Some(Ok(n)) => {
            buf.truncate(n);
            Ok(ok_wrap(Value::Bytes(Box::new(
                zz_runtime::BytesData::from_vec(buf),
            ))))
        }
        Some(Err(e)) => Ok(tls_err(&format!("tls:io: read failed ({e})"))),
        None => Err(gone("std.net.tls_read_bytes", span)),
    }
}

/// `tls.read_exact(n) -> Result<bytes, str>` — loops until `n` bytes (RESP bulk reads).
pub(crate) fn tls_read_exact(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_read_exact", span)?;
    let n = expect_int(args, 1, "std.net.tls_read_exact")?;
    if n <= 0 || n > 64 * 1024 * 1024 {
        return Ok(tls_err(&format!(
            "tls:io: bad read length {n} (want 1..67108864)"
        )));
    }
    let mut out = vec![0u8; n as usize];
    match zz_native_rt::tls_conn::with(h.id, |c| c.stream.read_exact(&mut out)) {
        Some(Ok(())) => Ok(ok_wrap(Value::Bytes(Box::new(
            zz_runtime::BytesData::from_vec(out),
        )))),
        Some(Err(e)) => Ok(tls_err(&format!("tls:io: read_exact failed ({e})"))),
        None => Err(gone("std.net.tls_read_exact", span)),
    }
}

pub(crate) fn tls_readline(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_readline", span)?;
    let mut line: Vec<u8> = Vec::new();
    let mut one = [0u8; 1];
    loop {
        let r = zz_native_rt::tls_conn::with(h.id, |c| c.stream.read_exact(&mut one));
        match r {
            Some(Ok(())) => {
                if one[0] == b'\n' {
                    break;
                }
                line.push(one[0]);
                if line.len() > 64 * 1024 {
                    return Ok(tls_err("tls:io: line too long (max 64 KiB)"));
                }
            }
            Some(Err(e)) => {
                if line.is_empty() {
                    return Ok(tls_err(&format!("tls:io: readline failed ({e})")));
                }
                break;
            }
            None => return Err(gone("std.net.tls_readline", span)),
        }
    }
    if !line.is_empty() && line[line.len() - 1] == b'\r' {
        line.pop();
    }
    Ok(ok_wrap(Value::Str(
        String::from_utf8_lossy(&line).into_owned().into(),
    )))
}

pub(crate) fn tls_set_read_timeout(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_set_read_timeout", span)?;
    let ms = expect_int(args, 1, "std.net.tls_set_read_timeout")?;
    let dur = if ms <= 0 {
        None
    } else {
        Some(std::time::Duration::from_millis(ms as u64))
    };
    match zz_native_rt::tls_conn::with(h.id, |c| c.set_read_timeout(dur)) {
        Some(Ok(())) => Ok(ok_wrap(Value::Bool(true))),
        Some(Err(e)) => Ok(tls_err(&format!("tls:io: set_read_timeout failed ({e})"))),
        None => Err(gone("std.net.tls_set_read_timeout", span)),
    }
}

pub(crate) fn tls_set_write_timeout(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_set_write_timeout", span)?;
    let ms = expect_int(args, 1, "std.net.tls_set_write_timeout")?;
    let dur = if ms <= 0 {
        None
    } else {
        Some(std::time::Duration::from_millis(ms as u64))
    };
    match zz_native_rt::tls_conn::with(h.id, |c| c.set_write_timeout(dur)) {
        Some(Ok(())) => Ok(ok_wrap(Value::Bool(true))),
        Some(Err(e)) => Ok(tls_err(&format!("tls:io: set_write_timeout failed ({e})"))),
        None => Err(gone("std.net.tls_set_write_timeout", span)),
    }
}

pub(crate) fn tls_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_close", span)?;
    zz_native_rt::tls_conn::with(h.id, |c| {
        let _ = c.shutdown();
    });
    zz_native_rt::drop_handle(h.id);
    Ok(ok_wrap(Value::Unit))
}

pub(crate) fn tls_peer_addr(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_tls(args, 0, "std.net.tls_peer_addr", span)?;
    match zz_native_rt::tls_conn::with(h.id, |c| c.peer.clone()) {
        Some(p) => Ok(ok_wrap(Value::Str(p.into()))),
        None => Err(gone("std.net.tls_peer_addr", span)),
    }
}

/// `net.tls_listen(addr, cert_file, key_file) -> Result<tls_listener, str>`
pub(crate) fn tls_listen(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let addr = expect_str(args, 0, "std.net.tls_listen")?;
    let cert = expect_str(args, 1, "std.net.tls_listen")?;
    let key = expect_str(args, 2, "std.net.tls_listen")?;
    match zz_native_rt::tls_conn::listen(&addr, &cert, &key) {
        Ok(h) => Ok(Value::Result(Box::new(Ok(Value::Opaque(Box::new(h)))))),
        Err(msg) => Ok(Value::Result(Box::new(Err(Value::Str(msg.into()))))),
    }
}

/// `net.tls_accept(listener) -> Result<tls, str>` (server-side handshake).
pub(crate) fn tls_accept(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let id = match arg(args, 0, "std.net.tls_accept")? {
        Value::Opaque(h) if h.tag == zz_native_rt::tls_conn::LISTENER_TAG => h.id,
        other => {
            return Err(EvalError::new(
                format!("`std.net.tls_accept` expects a tls listener, found `{other}`"),
                span,
            ))
        }
    };
    match zz_native_rt::tls_conn::accept(id) {
        Ok(h) => Ok(Value::Result(Box::new(Ok(Value::Opaque(Box::new(h)))))),
        Err(msg) => Ok(Value::Result(Box::new(Err(Value::Str(msg.into()))))),
    }
}

/// `tls_listener.close(listener)`.
pub(crate) fn tls_listener_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let id = match arg(args, 0, "std.net.tls_listener_close")? {
        Value::Opaque(h) if h.tag == zz_native_rt::tls_conn::LISTENER_TAG => h.id,
        other => {
            return Err(EvalError::new(
                format!("`std.net.tls_listener_close` expects a tls listener, found `{other}`"),
                span,
            ))
        }
    };
    zz_native_rt::drop_handle(id);
    Ok(Value::Result(Box::new(Ok(Value::Bool(true)))))
}
