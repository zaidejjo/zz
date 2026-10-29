//! `net.unix_connect` — Unix-domain socket client shared by VM and AOT.
//!
//! Same shape as `tls_conn` (Opaque handle + staticlib FFI, no C-tag
//! changes): the VM calls the safe API through thin `zz_stdlib`
//! adapters, AOT calls the `extern "C"` `zz_unix_*` functions.
//!
//! Semantics: SOCK_STREAM over a filesystem path. `connect` itself is
//! blocking (a missing socket fails fast with ENOENT/CONNREFUSED; only
//! a saturated listener backlog could stall — loopback-scoped and
//! documented). IO timeouts apply post-connect. Errors carry reason
//! classes: `refused | timeout | closed | io`, prefixed `unix:<reason>:`.

use std::io::{Read, Write};
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libc::{poll, pollfd, POLLIN, POLLOUT};

use crate::cabi::{cvalue_bytes, cvalue_str, cvalue_to_bytes, cvalue_to_string, CValue};
use crate::{alloc, payload, Handle};

/// Pool tag for Unix client connections. Selects the `unix.*` method namespace.
pub const TAG: &str = "unix";
/// Pool tag for Unix listeners (`unix_listener.*`: `accept`, `close`).
pub const LISTENER_TAG: &str = "unix_listener";

/// A live Unix-domain connection.
///
/// IO timeouts are applied in userspace with `poll(2)` before each
/// blocking read/write: Darwin rejects `SO_RCVTIMEO`/`SO_SNDTIMEO` on
/// `AF_UNIX` sockets (`EINVAL`), so the kernel cannot be relied on to
/// enforce them. The timeout is stored here and also handed to the kernel
/// as a best-effort hint (harmless where supported).
pub struct UnixConn {
    pub stream: UnixStream,
    pub peer: String,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

/// Wait up to `timeout` for the socket to be readable (or writable when
/// `write` is true). Returns `io::ErrorKind::TimedOut` ("timed out") on
/// expiry so `classify` maps it to `unix:timeout:`.
fn poll_ready(stream: &UnixStream, write: bool, timeout: Option<Duration>) -> std::io::Result<()> {
    let Some(dur) = timeout else { return Ok(()) };
    // Round up so a sub-millisecond timeout still polls at least 1 ms.
    let mut ms = dur.as_millis() as i32;
    if ms < 1 {
        ms = 1;
    }
    let mut pfd = pollfd {
        fd: stream.as_raw_fd(),
        events: if write { POLLOUT } else { POLLIN },
        revents: 0,
    };
    loop {
        let r = unsafe { poll(&mut pfd, 1, ms) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if r == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out",
            ));
        }
        return Ok(());
    }
}

impl UnixConn {
    /// Store an IO timeout (best-effort kernel hint too) and enforce it in
    /// userspace via `poll` on every read/write.
    pub fn set_read_timeout(&mut self, dur: Option<Duration>) -> std::io::Result<()> {
        let _ = self.stream.set_read_timeout(dur);
        self.read_timeout = dur;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, dur: Option<Duration>) -> std::io::Result<()> {
        let _ = self.stream.set_write_timeout(dur);
        self.write_timeout = dur;
        Ok(())
    }

    pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        poll_ready(&self.stream, false, self.read_timeout)?;
        self.stream.read(buf)
    }

    pub fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        let mut pos = 0;
        while pos < buf.len() {
            poll_ready(&self.stream, false, self.read_timeout)?;
            match self.stream.read(&mut buf[pos..]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "failed to fill whole buffer",
                    ))
                }
                Ok(n) => pos += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let mut pos = 0;
        while pos < buf.len() {
            poll_ready(&self.stream, true, self.write_timeout)?;
            match self.stream.write(&buf[pos..]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "write zero bytes",
                    ))
                }
                Ok(n) => pos += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Read until `\n` (inclusive), returning the line without it.
    /// `UnexpectedEof` with an empty line means the peer closed first.
    pub fn read_line(&mut self) -> std::io::Result<String> {
        let mut line: Vec<u8> = Vec::new();
        let mut one = [0u8; 1];
        loop {
            match self.read_exact(&mut one) {
                Ok(()) => {
                    if one[0] == b'\n' {
                        break;
                    }
                    line.push(one[0]);
                    if line.len() > 64 * 1024 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "line too long (max 64 KiB)",
                        ));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    if line.is_empty() {
                        return Err(e);
                    }
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Ok(String::from_utf8_lossy(&line).into_owned())
    }
}

fn classify(path: &str, e: &str) -> String {
    let low = e.to_lowercase();
    if low.contains("timed out")
        || low.contains("timeout")
        || low.contains("wouldblock")
        || low.contains("would block")
        || low.contains("temporarily unavailable")
    {
        return format!("unix:timeout: IO with `{path}` timed out ({e})");
    }
    if low.contains("refused")
        || low.contains("no such file")
        || low.contains("not found")
        || low.contains("not connected")
    {
        return format!("unix:refused: cannot connect to `{path}` ({e})");
    }
    if low.contains("closed")
        || low.contains("broken pipe")
        || low.contains("end of file")
        || low.contains("unexpected eof")
    {
        return format!("unix:closed: connection to `{path}` closed ({e})");
    }
    format!("unix:io: IO with `{path}` failed ({e})")
}

/// Connect to the socket at `path`. `timeout_ms` bounds IO after connect
/// (the connect itself fails fast on missing sockets).
pub fn connect(path: &str, timeout_ms: u64) -> Result<Handle, String> {
    if path.is_empty() {
        return Err("unix:refused: empty socket path".to_string());
    }
    let timeout = if timeout_ms == 0 {
        Duration::from_secs(30)
    } else {
        Duration::from_millis(timeout_ms)
    };
    let stream = UnixStream::connect(path).map_err(|e| classify(path, &e.to_string()))?;
    let mut conn = UnixConn {
        stream,
        peer: path.to_string(),
        read_timeout: None,
        write_timeout: None,
    };
    let _ = conn.set_read_timeout(Some(timeout));
    let _ = conn.set_write_timeout(Some(timeout));
    Ok(alloc(TAG, Arc::new(Mutex::new(conn))))
}

/// Run `f` on the connection for `id`. `None` for unknown/dropped ids and
/// for handles owned by another module (tag mismatch).
pub fn with<T>(id: u64, f: impl FnOnce(&mut UnixConn) -> T) -> Option<T> {
    payload::<Mutex<UnixConn>>(id, TAG).map(|m| {
        let mut lock = m.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut lock)
    })
}

/// A Unix listener (accept per connection, loopback tests).
pub struct UnixListenerState {
    listener: UnixListener,
}

/// Bind a Unix listener at `path` (removes a stale socket file first).
pub fn listen(path: &str) -> Result<Handle, String> {
    if path.is_empty() {
        return Err("unix:refused: empty socket path".to_string());
    }
    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).map_err(|e| format!("unix:io: bind `{path}` failed ({e})"))?;
    Ok(alloc(
        LISTENER_TAG,
        Arc::new(Mutex::new(UnixListenerState { listener })),
    ))
}

/// Accept one connection (blocking).
pub fn accept(id: u64) -> Result<Handle, String> {
    let listener = payload::<Mutex<UnixListenerState>>(id, LISTENER_TAG)
        .map(|m| {
            let guard = m.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .listener
                .try_clone()
                .map_err(|e| format!("unix:io: accept failed ({e})"))
        })
        .ok_or_else(|| "unix:io: not a unix listener".to_string())??;
    let (sock, _) = listener
        .accept()
        .map_err(|e| format!("unix:io: accept failed ({e})"))?;
    let mut conn = UnixConn {
        stream: sock,
        peer: String::new(),
        read_timeout: None,
        write_timeout: None,
    };
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = conn.set_write_timeout(Some(Duration::from_secs(10)));
    Ok(alloc(TAG, Arc::new(Mutex::new(conn))))
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        unsafe {
            *err = 1;
        }
    }
}

fn ok_str(s: &[u8]) -> CValue {
    unsafe { crate::cabi::zz_variant_ok(cvalue_str(s)) }
}

fn err_str(msg: String) -> CValue {
    unsafe { crate::cabi::zz_variant_err(cvalue_str(msg.as_bytes())) }
}

/// `net.unix_connect(path, timeout_ms) -> Result<int, str>` (AOT: handle id).
#[no_mangle]
pub extern "C" fn zz_unix_connect(
    path: CValue,
    timeout: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let Some(p) = cvalue_to_string(path) else {
        set_err(err);
        return err_str("unix:refused: expected (path: str, timeout_ms: int)".to_string());
    };
    let ms = timeout.as_i64().unwrap_or(5000).max(0) as u64;
    match connect(&p, ms) {
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => err_str(msg),
    }
}

/// `net.unix_listen(path) -> Result<int, str>`.
#[no_mangle]
pub extern "C" fn zz_unix_listen(path: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(p) = cvalue_to_string(path) else {
        set_err(err);
        return err_str("unix:refused: expected (path: str)".to_string());
    };
    match listen(&p) {
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => err_str(msg),
    }
}

/// `unix_listener.accept(id) -> Result<int, str>`.
#[no_mangle]
pub extern "C" fn zz_unix_accept(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("unix:io: expected (unix_listener)".to_string());
    };
    match accept(n as u64) {
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => err_str(msg),
    }
}

/// `unix_listener.close(id)`.
#[no_mangle]
pub extern "C" fn zz_unix_listener_close(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("unix:io: expected (unix_listener)".to_string());
    };
    crate::drop_handle(n as u64);
    unsafe { crate::cabi::zz_variant_ok(CValue::boolean(true)) }
}

/// `unix.write(id, data: str) -> Result<int, str>`.
#[no_mangle]
pub extern "C" fn zz_unix_write(id: CValue, data: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(n), Some(text)) = (id.as_i64(), cvalue_to_string(data)) else {
        set_err(err);
        return err_str("unix:io: expected (unix, data: str)".to_string());
    };
    match with(n as u64, |c| c.write_all(text.as_bytes())) {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(CValue::int(text.len() as i64)) },
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("unix:io: not a unix connection".to_string())
        }
    }
}

/// `unix.write_bytes(id, data: bytes) -> Result<int, str>`.
#[no_mangle]
pub extern "C" fn zz_unix_write_bytes(
    id: CValue,
    data: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(n), Some(bytes)) = (id.as_i64(), cvalue_to_bytes(data)) else {
        set_err(err);
        return err_str("unix:io: expected (unix, data: bytes)".to_string());
    };
    let len = bytes.len();
    match with(n as u64, |c| c.write_all(&bytes)) {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(CValue::int(len as i64)) },
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("unix:io: not a unix connection".to_string())
        }
    }
}

/// `unix.read(id, max_bytes) -> Result<str, str>` (single read).
#[no_mangle]
pub extern "C" fn zz_unix_read(id: CValue, max: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(n), Some(cap)) = (id.as_i64(), max.as_i64()) else {
        set_err(err);
        return err_str("unix:io: expected (unix, max_bytes: int)".to_string());
    };
    let cap = cap.clamp(1, 8 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; cap];
    match with(n as u64, |c| c.read(&mut buf)) {
        Some(Ok(0)) => err_str("unix:closed: connection closed".to_string()),
        Some(Ok(got)) => {
            buf.truncate(got);
            let s = String::from_utf8_lossy(&buf).into_owned();
            ok_str(s.as_bytes())
        }
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("unix:io: not a unix connection".to_string())
        }
    }
}

/// `unix.read_bytes(id, max_bytes) -> Result<bytes, str>`.
#[no_mangle]
pub extern "C" fn zz_unix_read_bytes(id: CValue, max: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(n), Some(cap)) = (id.as_i64(), max.as_i64()) else {
        set_err(err);
        return err_str("unix:io: expected (unix, max_bytes: int)".to_string());
    };
    let cap = cap.clamp(1, 8 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; cap];
    match with(n as u64, |c| c.read(&mut buf)) {
        Some(Ok(0)) => err_str("unix:closed: connection closed".to_string()),
        Some(Ok(got)) => {
            buf.truncate(got);
            unsafe { crate::cabi::zz_variant_ok(cvalue_bytes(&buf)) }
        }
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("unix:io: not a unix connection".to_string())
        }
    }
}

/// `unix.read_exact(id, n) -> Result<bytes, str>`.
#[no_mangle]
pub extern "C" fn zz_unix_read_exact(
    id: CValue,
    want: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(n), Some(k)) = (id.as_i64(), want.as_i64()) else {
        set_err(err);
        return err_str("unix:io: expected (unix, n: int)".to_string());
    };
    if k <= 0 || k > 64 * 1024 * 1024 {
        set_err(err);
        return err_str(format!("unix:io: bad read length {k} (want 1..67108864)"));
    }
    let k = k as usize;
    let mut out = vec![0u8; k];
    let res = with(n as u64, |c| c.read_exact(&mut out));
    match res {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(cvalue_bytes(&out)) },
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("unix:io: not a unix connection".to_string())
        }
    }
}

/// `unix.read_line(id) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_unix_read_line(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("unix:io: expected (unix)".to_string());
    };
    let mut line: Vec<u8> = Vec::new();
    let mut one = [0u8; 1];
    loop {
        let r = with(n as u64, |c| c.read_exact(&mut one));
        match r {
            Some(Ok(())) => {
                if one[0] == b'\n' {
                    break;
                }
                line.push(one[0]);
                if line.len() > 64 * 1024 {
                    return err_str("unix:io: line too long (max 64 KiB)".to_string());
                }
            }
            Some(Err(e)) => {
                if line.is_empty() {
                    return err_str(classify("peer", &e.to_string()));
                }
                break;
            }
            None => {
                set_err(err);
                return err_str("unix:io: not a unix connection".to_string());
            }
        }
    }
    if !line.is_empty() && line[line.len() - 1] == b'\r' {
        line.pop();
    }
    let s = String::from_utf8_lossy(&line).into_owned();
    ok_str(s.as_bytes())
}

/// `unix.set_read_timeout(id, ms)` / `unix.set_write_timeout(id, ms)`.
#[no_mangle]
pub extern "C" fn zz_unix_set_read_timeout(
    id: CValue,
    ms: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(n), Some(v)) = (id.as_i64(), ms.as_i64()) else {
        set_err(err);
        return err_str("unix:io: expected (unix, ms: int)".to_string());
    };
    let dur = if v <= 0 {
        None
    } else {
        Some(Duration::from_millis(v as u64))
    };
    match with(n as u64, |c| c.set_read_timeout(dur)) {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(CValue::boolean(true)) },
        Some(Err(e)) => err_str(format!("unix:io: set_read_timeout failed ({e})")),
        None => {
            set_err(err);
            err_str("unix:io: not a unix connection".to_string())
        }
    }
}

/// `unix.set_write_timeout(id, ms)`.
#[no_mangle]
pub extern "C" fn zz_unix_set_write_timeout(
    id: CValue,
    ms: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(n), Some(v)) = (id.as_i64(), ms.as_i64()) else {
        set_err(err);
        return err_str("unix:io: expected (unix, ms: int)".to_string());
    };
    let dur = if v <= 0 {
        None
    } else {
        Some(Duration::from_millis(v as u64))
    };
    match with(n as u64, |c| c.set_write_timeout(dur)) {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(CValue::boolean(true)) },
        Some(Err(e)) => err_str(format!("unix:io: set_write_timeout failed ({e})")),
        None => {
            set_err(err);
            err_str("unix:io: not a unix connection".to_string())
        }
    }
}

/// `unix.close(id)` — shutdown + drop the handle.
#[no_mangle]
pub extern "C" fn zz_unix_close(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("unix:io: expected (unix)".to_string());
    };
    with(n as u64, |c| {
        let _ = c.stream.shutdown(std::net::Shutdown::Both);
    });
    crate::drop_handle(n as u64);
    unsafe { crate::cabi::zz_variant_ok(CValue::unit()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_names_reasons() {
        assert!(classify("p", "connection refused").starts_with("unix:refused:"));
        assert!(classify("p", "no such file").starts_with("unix:refused:"));
        assert!(classify("p", "timed out").starts_with("unix:timeout:"));
        assert!(classify("p", "broken pipe").starts_with("unix:closed:"));
        assert!(classify("p", "boom").starts_with("unix:io:"));
    }

    #[test]
    fn rejects_empty_path() {
        assert!(connect("", 200).unwrap_err().starts_with("unix:refused:"));
        assert!(listen("").unwrap_err().starts_with("unix:refused:"));
    }

    #[test]
    fn loopback_echo() {
        let path = std::env::temp_dir().join(format!("zz_unix_test_{}.sock", std::process::id()));
        let path = path.to_string_lossy().into_owned();
        let lh = listen(&path).expect("listen");
        let addr = path.clone();
        let h = std::thread::spawn(move || accept(lh.id));
        let c = connect(&addr, 5000).expect("connect");
        let s = h.join().unwrap().expect("accept");
        with(c.id, |cc| cc.stream.write_all(b"hello"))
            .unwrap()
            .unwrap();
        with(s.id, |ss| {
            let mut buf = [0u8; 5];
            ss.stream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"hello");
            ss.stream.write_all(b"world").unwrap();
        });
        with(c.id, |cc| {
            let mut buf = [0u8; 5];
            cc.stream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"world");
        });
        // Missing socket fails fast with refused.
        let err = connect("/nonexistent-dir-zz/no.sock", 2000).unwrap_err();
        assert!(err.starts_with("unix:refused:"), "{err}");
        crate::drop_handle(c.id);
        crate::drop_handle(s.id);
        let _ = std::fs::remove_file(&path);
    }
}
