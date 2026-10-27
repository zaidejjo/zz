//! `net.unix_connect` — Windows stub.
//!
//! Unix-domain sockets exist on Windows 10+ (`AF_UNIX`), but Rust's std
//! exposes them only through `std::os::unix`, which does not exist on
//! Windows. Rather than cfg-gating every caller (VM natives, AOT FFI
//! registry, method-tag dispatch), this module keeps the public surface
//! identical to [`super::unix_conn`] and fails every operation with a
//! clear `unix:unsupported:` error. Programs that never call the
//! `net.unix_*` natives are unaffected; programs that do get a
//! diagnostic instead of a missing-symbol link error.

use std::io::{Read, Write};
use std::time::Duration;

use crate::cabi::{cvalue_str, CValue};
use crate::Handle;

/// Pool tag for Unix client connections. Selects the `unix.*` method namespace.
pub const TAG: &str = "unix";
/// Pool tag for Unix listeners (`unix_listener.*`: `accept`, `close`).
pub const LISTENER_TAG: &str = "unix_listener";

const UNSUPPORTED: &str =
    "unix:unsupported: Unix-domain sockets are not available in this Windows build";

/// Placeholder stream: never produced by [`connect`] (which always errors),
/// so all IO methods are unreachable. They exist to keep the VM adapters
/// source-identical across platforms.
pub struct StubStream;

impl Read for StubStream {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            UNSUPPORTED,
        ))
    }
}

impl Write for StubStream {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            UNSUPPORTED,
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            UNSUPPORTED,
        ))
    }
}

impl StubStream {
    pub fn shutdown(&self, _how: std::net::Shutdown) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            UNSUPPORTED,
        ))
    }

    pub fn set_read_timeout(&self, _dur: Option<Duration>) -> std::io::Result<()> {
        Ok(())
    }

    pub fn set_write_timeout(&self, _dur: Option<Duration>) -> std::io::Result<()> {
        Ok(())
    }
}

/// A live Unix-domain connection (never constructed on Windows).
pub struct UnixConn {
    pub stream: StubStream,
    pub peer: String,
}

/// A Unix listener (never constructed on Windows).
pub struct UnixListenerState;

/// Always fails: Unix-domain sockets are unsupported in this build.
pub fn connect(_path: &str, _timeout_ms: u64) -> Result<Handle, String> {
    Err(UNSUPPORTED.to_string())
}

/// Always fails: Unix-domain sockets are unsupported in this build.
pub fn listen(_path: &str) -> Result<Handle, String> {
    Err(UNSUPPORTED.to_string())
}

/// Always fails: Unix-domain sockets are unsupported in this build.
pub fn accept(_id: u64) -> Result<Handle, String> {
    Err(UNSUPPORTED.to_string())
}

/// No payload exists on Windows, so this always returns `None`.
pub fn with<T>(_id: u64, _f: impl FnOnce(&mut UnixConn) -> T) -> Option<T> {
    None
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        unsafe {
            *err = 1;
        }
    }
}

fn err_str(err: *mut std::ffi::c_int) -> CValue {
    set_err(err);
    unsafe { crate::cabi::zz_variant_err(cvalue_str(UNSUPPORTED.as_bytes())) }
}

/// `net.unix_connect(path, timeout_ms)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_connect(
    _path: CValue,
    _timeout: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    err_str(err)
}

/// `net.unix_listen(path)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_listen(_path: CValue, err: *mut std::ffi::c_int) -> CValue {
    err_str(err)
}

/// `unix_listener.accept(id)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_accept(_id: CValue, err: *mut std::ffi::c_int) -> CValue {
    err_str(err)
}

/// `unix_listener.close(id)` — drop the handle (no socket to close).
#[no_mangle]
pub extern "C" fn zz_unix_listener_close(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        return err_str(err);
    };
    crate::drop_handle(n as u64);
    unsafe { crate::cabi::zz_variant_ok(CValue::boolean(true)) }
}

/// `unix.write(id, data)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_write(_id: CValue, _data: CValue, err: *mut std::ffi::c_int) -> CValue {
    err_str(err)
}

/// `unix.write_bytes(id, data)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_write_bytes(
    _id: CValue,
    _data: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    err_str(err)
}

/// `unix.read(id, max_bytes)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_read(_id: CValue, _max: CValue, err: *mut std::ffi::c_int) -> CValue {
    err_str(err)
}

/// `unix.read_bytes(id, max_bytes)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_read_bytes(
    _id: CValue,
    _max: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    err_str(err)
}

/// `unix.read_exact(id, n)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_read_exact(
    _id: CValue,
    _want: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    err_str(err)
}

/// `unix.read_line(id)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_read_line(_id: CValue, err: *mut std::ffi::c_int) -> CValue {
    err_str(err)
}

/// `unix.set_read_timeout(id, ms)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_set_read_timeout(
    _id: CValue,
    _ms: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    err_str(err)
}

/// `unix.set_write_timeout(id, ms)` — unsupported on Windows.
#[no_mangle]
pub extern "C" fn zz_unix_set_write_timeout(
    _id: CValue,
    _ms: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    err_str(err)
}

/// `unix.close(id)` — drop the handle.
#[no_mangle]
pub extern "C" fn zz_unix_close(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        return err_str(err);
    };
    crate::drop_handle(n as u64);
    unsafe { crate::cabi::zz_variant_ok(CValue::unit()) }
}
