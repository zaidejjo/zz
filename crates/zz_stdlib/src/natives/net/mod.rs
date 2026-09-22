//! `std.net` — Low-level TCP networking.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::natives::{arg, expect_int, expect_str};
use zz_runtime::{EvalError, Interp, Span, Value};

/// `net.tcp_connect(addr: str, timeout_ms: int) -> Result<tcp.stream, str>`
pub(crate) fn tcp_connect(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    use std::net::ToSocketAddrs;
    let addr = expect_str(args, 0, "std.net.tcp_connect")?;
    let timeout_ms = expect_int(args, 1, "std.net.tcp_connect")? as u64;
    let timeout = Duration::from_millis(timeout_ms);
    let socket_addr = match addr.to_socket_addrs() {
        Ok(mut addrs) => addrs.next().ok_or_else(|| {
            EvalError::new(format!("no addresses found for `{addr}`"), Span::new(0, 0))
        })?,
        Err(e) => {
            return Ok(Value::Result(Box::new(Err(Value::Str(
                format!("invalid address: {e}").into(),
            )))))
        }
    };
    match TcpStream::connect_timeout(&socket_addr, timeout) {
        Ok(stream) => Ok(Value::Result(Box::new(Ok(Value::TcpStream(Arc::new(
            Mutex::new(stream),
        )))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("tcp_connect failed: {e}").into(),
        ))))),
    }
}

/// `net.tcp_listen(addr: str) -> Result<tcp.listener, str>`
pub(crate) fn tcp_listen(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let addr = expect_str(args, 0, "std.net.tcp_listen")?;
    match std::net::TcpListener::bind(&addr) {
        Ok(listener) => Ok(Value::Result(Box::new(Ok(Value::TcpListener(Arc::new(
            Mutex::new(listener),
        )))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("tcp_listen failed: {e}").into(),
        ))))),
    }
}

/// `net.tcp_accept(listener: tcp.listener) -> Result<tcp.stream, str>`
pub(crate) fn tcp_accept(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let listener = match arg(args, 0, "std.net.tcp_accept")? {
        Value::TcpListener(l) => l.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.tcp_accept: expected a tcp.listener, found `{other}`"),
                span,
            ))
        }
    };
    let lock = listener
        .lock()
        .map_err(|e| EvalError::new(format!("std.net.tcp_accept: lock poisoned: {e}"), span))?;
    match lock.accept() {
        Ok((stream, _addr)) => Ok(Value::Result(Box::new(Ok(Value::TcpStream(Arc::new(
            Mutex::new(stream),
        )))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("tcp_accept failed: {e}").into(),
        ))))),
    }
}

/// `net.tcp_write(stream: tcp.stream, data: str) -> Result<int, str>`
pub(crate) fn tcp_write(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let stream = match arg(args, 0, "std.net.tcp_write")? {
        Value::TcpStream(s) => s.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.tcp_write: expected a tcp.stream, found `{other}`"),
                span,
            ))
        }
    };
    let data = expect_str(args, 1, "std.net.tcp_write")?;
    let mut lock = stream
        .lock()
        .map_err(|e| EvalError::new(format!("std.net.tcp_write: lock poisoned: {e}"), span))?;
    match lock.write_all(data.as_bytes()) {
        Ok(()) => {
            let len = data.len() as i64;
            Ok(Value::Result(Box::new(Ok(Value::Int(len)))))
        }
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("tcp_write failed: {e}").into(),
        ))))),
    }
}

/// `net.tcp_read(stream: tcp.stream, max_bytes: int) -> Result<str, str>`
pub(crate) fn tcp_read(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let stream = match arg(args, 0, "std.net.tcp_read")? {
        Value::TcpStream(s) => s.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.tcp_read: expected a tcp.stream, found `{other}`"),
                span,
            ))
        }
    };
    let max_bytes = expect_int(args, 1, "std.net.tcp_read")? as usize;
    let mut lock = stream
        .lock()
        .map_err(|e| EvalError::new(format!("std.net.tcp_read: lock poisoned: {e}"), span))?;
    let mut buf = vec![0u8; max_bytes];
    match lock.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            let s = String::from_utf8_lossy(&buf).to_string();
            Ok(Value::Result(Box::new(Ok(Value::Str(s.into())))))
        }
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("tcp_read failed: {e}").into(),
        ))))),
    }
}

/// `net.tcp_readline(stream: tcp.stream) -> Result<str, str>`
///
/// Read until `\n` (inclusive). Returns the line without the trailing newline.
pub(crate) fn tcp_readline(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let stream = match arg(args, 0, "std.net.tcp_readline")? {
        Value::TcpStream(s) => s.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.tcp_readline: expected a tcp.stream, found `{other}`"),
                span,
            ))
        }
    };
    let mut lock = stream
        .lock()
        .map_err(|e| EvalError::new(format!("std.net.tcp_readline: lock poisoned: {e}"), span))?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match lock.read_exact(&mut byte) {
            Ok(()) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                if line.is_empty() {
                    return Ok(Value::Result(Box::new(Err(Value::Str(
                        "connection closed".to_string().into(),
                    )))));
                }
                break;
            }
            Err(e) => {
                return Ok(Value::Result(Box::new(Err(Value::Str(
                    format!("tcp_readline failed: {e}").into(),
                )))))
            }
        }
    }
    let s = String::from_utf8_lossy(&line).to_string();
    Ok(Value::Result(Box::new(Ok(Value::Str(s.into())))))
}

/// `net.tcp_close(stream: tcp.stream) -> Result<bool, str>`
///
/// Note: TCP streams are automatically closed when dropped.
/// This function exists for API completeness and returns true.
pub(crate) fn tcp_close(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    // The stream will be closed when the last Arc reference is dropped.
    Ok(Value::Result(Box::new(Ok(Value::Bool(true)))))
}

/// `net.peer_addr(stream: tcp.stream) -> Result<str, str>`
pub(crate) fn peer_addr(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let stream = match arg(args, 0, "std.net.peer_addr")? {
        Value::TcpStream(s) => s.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.peer_addr: expected a tcp.stream, found `{other}`"),
                span,
            ))
        }
    };
    let lock = stream
        .lock()
        .map_err(|e| EvalError::new(format!("std.net.peer_addr: lock poisoned: {e}"), span))?;
    match lock.peer_addr() {
        Ok(addr) => Ok(Value::Result(Box::new(Ok(Value::Str(
            addr.to_string().into(),
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("peer_addr failed: {e}").into(),
        ))))),
    }
}

/// `net.local_addr(stream: tcp.stream) -> Result<str, str>`
pub(crate) fn local_addr(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let stream = match arg(args, 0, "std.net.local_addr")? {
        Value::TcpStream(s) => s.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.local_addr: expected a tcp.stream, found `{other}`"),
                span,
            ))
        }
    };
    let lock = stream
        .lock()
        .map_err(|e| EvalError::new(format!("std.net.local_addr: lock poisoned: {e}"), span))?;
    match lock.local_addr() {
        Ok(addr) => Ok(Value::Result(Box::new(Ok(Value::Str(
            addr.to_string().into(),
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("local_addr failed: {e}").into(),
        ))))),
    }
}

/// `net.set_read_timeout(stream: tcp.stream, ms: int) -> Result<bool, str>`
pub(crate) fn set_read_timeout(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let stream = match arg(args, 0, "std.net.set_read_timeout")? {
        Value::TcpStream(s) => s.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.set_read_timeout: expected a tcp.stream, found `{other}`"),
                span,
            ))
        }
    };
    let ms = expect_int(args, 1, "std.net.set_read_timeout")? as u64;
    let lock = stream.lock().map_err(|e| {
        EvalError::new(
            format!("std.net.set_read_timeout: lock poisoned: {e}"),
            span,
        )
    })?;
    match lock.set_read_timeout(Some(Duration::from_millis(ms))) {
        Ok(()) => Ok(Value::Result(Box::new(Ok(Value::Bool(true))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("set_read_timeout failed: {e}").into(),
        ))))),
    }
}

/// `net.set_write_timeout(stream: tcp.stream, ms: int) -> Result<bool, str>`
pub(crate) fn set_write_timeout(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let stream = match arg(args, 0, "std.net.set_write_timeout")? {
        Value::TcpStream(s) => s.clone(),
        other => {
            return Err(EvalError::new(
                format!("std.net.set_write_timeout: expected a tcp.stream, found `{other}`"),
                span,
            ))
        }
    };
    let ms = expect_int(args, 1, "std.net.set_write_timeout")? as u64;
    let lock = stream.lock().map_err(|e| {
        EvalError::new(
            format!("std.net.set_write_timeout: lock poisoned: {e}"),
            span,
        )
    })?;
    match lock.set_write_timeout(Some(Duration::from_millis(ms))) {
        Ok(()) => Ok(Value::Result(Box::new(Ok(Value::Bool(true))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("set_write_timeout failed: {e}").into(),
        ))))),
    }
}
