//! `test.temp_dir` / `test.free_port` natives (VM side).
//!
//! Test-only helpers, also reachable bare (`temp_dir`, `free_port`) like the
//! assertions. Both return `Result<_, str>` so setup failures surface with
//! `?`/match instead of aborting the whole runner:
//! - `temp_dir(prefix)` creates a unique private (`0700`) subdir under the
//!   OS temp dir and returns its path. Clean up with `fs.remove_dir_all`
//!   (usually in `@teardown`).
//! - `free_port()` binds `127.0.0.1:0`, reports the assigned port, and
//!   closes the socket. TOCTOU: re-bind immediately; retry on collision.

use std::sync::atomic::{AtomicU64, Ordering};

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::expect_str;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn ok_str(s: String) -> Value {
    Value::Result(Box::new(Ok(Value::Str(s.into()))))
}

fn err_str(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

/// Sanitize a user prefix into a single path component: separators and
/// NUL become `_`, `..` can never escape the temp base.
fn sanitize_prefix(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len());
    for ch in prefix.chars() {
        if ch == '/' || ch == '\\' || ch == '\0' {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() {
        out.push_str("zz-test");
    }
    // Collapse any `..` runs so the component can never climb.
    while out.contains("..") {
        out = out.replace("..", "__");
    }
    // Bound length: OS paths cap ~255 per component.
    if out.len() > 64 {
        out.truncate(64);
    }
    out
}

pub(crate) fn test_temp_dir(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let prefix = if args.is_empty() {
        "zz-test".to_string()
    } else {
        expect_str(args, 0, "test.temp_dir")?
    };
    let clean = sanitize_prefix(&prefix);
    let base = std::env::temp_dir();
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let name = format!("{}-{}-{}-{}", clean, std::process::id(), seq, nanos);
    let path = base.join(name);
    match std::fs::create_dir_all(&path) {
        Ok(()) => {
            // Best-effort private perms on Unix ( umask may widen; 0700 intent).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            }
            Ok(ok_str(path.to_string_lossy().into_owned()))
        }
        Err(e) => Ok(err_str(format!(
            "test.temp_dir:io_error: {} ({e})",
            path.display()
        ))),
    }
}

pub(crate) fn test_free_port(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => match l.local_addr() {
            Ok(addr) => Ok(Value::Result(Box::new(Ok(Value::Int(addr.port() as i64))))),
            Err(e) => Ok(err_str(format!("test.free_port:addr: ({e})"))),
        },
        Err(e) => Ok(err_str(format!("test.free_port:bind: ({e})"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val_to_string(v: &Value) -> String {
        v.to_string()
    }

    #[test]
    fn temp_dir_creates_private_dir() {
        let mut interp = Interp::new();
        let mut args = vec![Value::Str("zz-unit".to_string().into())];
        let v = test_temp_dir(&mut interp, &mut args, Span::new(0, 0)).unwrap();
        let s = val_to_string(&v);
        assert!(s.starts_with(".ok("), "expected .ok, got {s}");
        let path = s.trim_start_matches(".ok(").trim_end_matches(')');
        assert!(std::path::Path::new(path).is_dir(), "missing dir {path}");
        // Prefix sanitization: separators never escape temp base.
        let mut evil = vec![Value::Str("../evil/x".to_string().into())];
        let v2 = test_temp_dir(&mut interp, &mut evil, Span::new(0, 0)).unwrap();
        let s2 = val_to_string(&v2);
        assert!(s2.starts_with(".ok("), "got {s2}");
        assert!(!s2.contains(".."), "unsanitized {s2}");
        std::fs::remove_dir_all(path).ok();
    }

    #[test]
    fn free_port_is_valid() {
        let mut interp = Interp::new();
        let mut no_args: Vec<Value> = vec![];
        let v = test_free_port(&mut interp, &mut no_args, Span::new(0, 0)).unwrap();
        let s = val_to_string(&v);
        assert!(s.starts_with(".ok("), "expected .ok, got {s}");
        let port: u16 = s
            .trim_start_matches(".ok(")
            .trim_end_matches(')')
            .parse()
            .expect("port");
        // Range only: re-binding here would be TOCTOU-flaky under parallel
        // libtest (kernel may hand the closed port to another thread first).
        assert!((1..=65535).contains(&port), "got {port}");
    }
}
