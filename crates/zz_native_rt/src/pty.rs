//! `std.pty` pseudo-terminal support for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! ([`pty_open`], [`pty_read`], [`pty_write`], [`pty_ptsname`],
//! [`pty_grantpt`], [`pty_unlockpt`], [`pty_set_winsz`], [`pty_isatty`])
//! through thin `zz_stdlib` adapters, while AOT binaries call the
//! `extern "C"` `zz_pty_*` functions (same `zz_value` convention as
//! `zz_term_*` / `zz_process_*`).
//!
//! Model:
//! - `pty_open` allocates a master/slave pair via `openpty`, returns
//!   `[master_fd, slave_path]`. The slave is not yet the controlling
//!   terminal; callers that need a controlling tty (e.g. `spawn_pty`)
//!   perform `setsid` + `TIOCSCTTY` in the child before `exec`.
//! - `pty_read`/`pty_write` stream bytes on the master fd. `read` blocks
//!   up to `n` bytes (lossy UTF-8 on the ZZ side); `write` returns the
//!   byte count. Both fail soft on closed/invalid fds.
//! - `pty_ptsname`/`grantpt`/`unlockpt` prepare the slave per POSIX.
//! - `pty_set_winsz` issues `TIOCSWINSZ` on a fd.
//! - `pty_isatty` is total.
//! - Unix-only; Windows/stub return `.err(unsupported)`, mirroring
//!   `term` raw mode.
//!
//! `spawn_pty` itself lives in `zz_stdlib::natives::process` (it needs the
//! `process` wait pool) and calls `setsid`/`TIOCSCTTY`/dup2 in the child
//! before `execvp`, keeping the master fd in the parent.

use crate::cabi::{cvalue_str, cvalue_to_string, CValue};

#[cfg(unix)]
mod unix {
    // `openpty(&mut master, &mut slave) -> libc::c_int`.
    //
    // `libc::openpty` is declared directly against the stable libc ABI
    // (not all targets export it via the `libc` crate).
    #[link(name = "c")]
    extern "C" {
        fn openpty(
            amaster: *mut libc::c_int,
            aslave: *mut libc::c_int,
            name: *mut libc::c_char,
            termp: *const libc::termios,
            winp: *const libc::winsize,
        ) -> libc::c_int;
    }

    pub fn pty_open() -> Result<(i32, i32), String> {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        // SAFETY: master/slave are valid out-params; name/termp/winp may be
        // null (openpty accepts null for term/winsize).
        let rc = unsafe {
            openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if rc != 0 {
            return Err(format!(
                "pty.open: openpty failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok((master, slave))
    }

    /// Read the slave path for an open master fd via `ptsname` (ptsname
    /// takes the master fd, per POSIX).
    pub fn pty_ptsname(fd: i32) -> Result<String, String> {
        // SAFETY: `ptsname` returns a pointer to static storage; copy it
        // immediately before any subsequent call clobbers it.
        let ptr = unsafe { libc::ptsname(fd) };
        if ptr.is_null() {
            return Err(format!(
                "pty.ptsname: ptsname failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let cstr = unsafe { std::ffi::CStr::from_ptr(ptr) };
        Ok(cstr.to_string_lossy().into_owned())
    }

    pub fn pty_grantpt(fd: i32) -> Result<(), String> {
        // SAFETY: `grantpt` takes only an fd.
        if unsafe { libc::grantpt(fd) } != 0 {
            return Err(format!(
                "pty.grantpt: grantpt failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub fn pty_unlockpt(fd: i32) -> Result<(), String> {
        // SAFETY: `unlockpt` takes only an fd.
        if unsafe { libc::unlockpt(fd) } != 0 {
            return Err(format!(
                "pty.unlockpt: unlockpt failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// `TIOCSWINSZ` on `fd` with `cols`/`rows`.
    pub fn pty_set_winsz(fd: i32, cols: i32, rows: i32) -> Result<(), String> {
        let ws = libc::winsize {
            ws_row: rows as libc::c_ushort,
            ws_col: cols as libc::c_ushort,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `ws` is a valid local; `TIOCSWINSZ` is correct for the fd.
        if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) } != 0 {
            return Err(format!(
                "pty.set_winsz: ioctl TIOCSWINSZ failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Read up to `n` bytes from `fd` (blocking until at least one byte or
    /// EOF), returning lossy UTF-8. A single `read` syscall — does not wait
    /// to fill `n`, mirroring `read_key`'s single-byte contract generalized.
    pub fn pty_read(fd: i32, n: usize) -> Result<String, String> {
        if n == 0 {
            return Ok(String::new());
        }
        let mut buf = vec![0u8; n.min(4096)];
        // SAFETY: `buf` is a valid buffer for the call.
        let got = loop {
            let r = unsafe {
                libc::read(
                    fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len() as libc::size_t,
                )
            };
            if r > 0 {
                break r as usize;
            }
            if r == 0 {
                // EOF.
                break 0;
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(format!("pty.read: read failed: {err}"));
        };
        Ok(String::from_utf8_lossy(&buf[..got]).into_owned())
    }

    /// Write `data` bytes to `fd`, returning the byte count.
    pub fn pty_write(fd: i32, data: &[u8]) -> Result<i64, String> {
        let mut written = 0usize;
        while written < data.len() {
            // SAFETY: `data[written..]` is a valid slice for the call.
            let n = unsafe {
                libc::write(
                    fd,
                    data.as_ptr().add(written) as *const libc::c_void,
                    (data.len() - written) as libc::size_t,
                )
            };
            if n > 0 {
                written += n as usize;
                continue;
            }
            if n == 0 {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(format!("pty.write: write failed: {err}"));
        }
        Ok(written as i64)
    }

    pub fn pty_isatty(fd: i32) -> bool {
        // SAFETY: `isatty` takes a bare fd, no pointers, no state.
        unsafe { libc::isatty(fd) == 1 }
    }
}

#[cfg(windows)]
mod windows {
    pub fn pty_open() -> Result<(i32, i32), String> {
        Err("pty.open: unsupported on this platform".to_string())
    }
    pub fn pty_ptsname(_fd: i32) -> Result<String, String> {
        Err("pty.ptsname: unsupported on this platform".to_string())
    }
    pub fn pty_grantpt(_fd: i32) -> Result<(), String> {
        Err("pty.grantpt: unsupported on this platform".to_string())
    }
    pub fn pty_unlockpt(_fd: i32) -> Result<(), String> {
        Err("pty.unlockpt: unsupported on this platform".to_string())
    }
    pub fn pty_set_winsz(_fd: i32, _cols: i32, _rows: i32) -> Result<(), String> {
        Err("pty.set_winsz: unsupported on this platform".to_string())
    }
    pub fn pty_read(_fd: i32, _n: usize) -> Result<String, String> {
        Err("pty.read: unsupported on this platform".to_string())
    }
    pub fn pty_write(_fd: i32, _data: &[u8]) -> Result<i64, String> {
        Err("pty.write: unsupported on this platform".to_string())
    }
    pub fn pty_isatty(_fd: i32) -> bool {
        false
    }
}

#[cfg(not(any(unix, windows)))]
mod stub {
    pub fn pty_open() -> Result<(i32, i32), String> {
        Err("pty.open: unsupported on this platform".to_string())
    }
    pub fn pty_ptsname(_fd: i32) -> Result<String, String> {
        Err("pty.ptsname: unsupported on this platform".to_string())
    }
    pub fn pty_grantpt(_fd: i32) -> Result<(), String> {
        Err("pty.grantpt: unsupported on this platform".to_string())
    }
    pub fn pty_unlockpt(_fd: i32) -> Result<(), String> {
        Err("pty.unlockpt: unsupported on this platform".to_string())
    }
    pub fn pty_set_winsz(_fd: i32, _cols: i32, _rows: i32) -> Result<(), String> {
        Err("pty.set_winsz: unsupported on this platform".to_string())
    }
    pub fn pty_read(_fd: i32, _n: usize) -> Result<String, String> {
        Err("pty.read: unsupported on this platform".to_string())
    }
    pub fn pty_write(_fd: i32, _data: &[u8]) -> Result<i64, String> {
        Err("pty.write: unsupported on this platform".to_string())
    }
    pub fn pty_isatty(_fd: i32) -> bool {
        false
    }
}

#[cfg(not(any(unix, windows)))]
use stub as imp;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
use windows as imp;

pub fn pty_open() -> Result<(i32, i32), String> {
    imp::pty_open()
}
pub fn pty_ptsname(fd: i32) -> Result<String, String> {
    imp::pty_ptsname(fd)
}
pub fn pty_grantpt(fd: i32) -> Result<(), String> {
    imp::pty_grantpt(fd)
}
pub fn pty_unlockpt(fd: i32) -> Result<(), String> {
    imp::pty_unlockpt(fd)
}
pub fn pty_set_winsz(fd: i32, cols: i32, rows: i32) -> Result<(), String> {
    imp::pty_set_winsz(fd, cols, rows)
}
pub fn pty_read(fd: i32, n: usize) -> Result<String, String> {
    imp::pty_read(fd, n)
}
pub fn pty_write(fd: i32, data: &[u8]) -> Result<i64, String> {
    imp::pty_write(fd, data)
}
pub fn pty_isatty(fd: i32) -> bool {
    imp::pty_isatty(fd)
}

fn ok_wrap(v: CValue) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_ok(v) }
}

fn err_wrap(msg: &str) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_err(cvalue_str(msg.as_bytes())) }
}

/// `pty.open() -> Result<[int, int], str>` — `[master_fd, slave_fd]`.
#[no_mangle]
pub extern "C" fn zz_pty_open(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    match pty_open() {
        Ok((master, slave)) => {
            // SAFETY: constructors are provided by the linked AOT program.
            unsafe {
                let arr = crate::cabi::zz_array_new();
                let ptr = arr.as_array_ptr().unwrap_or(std::ptr::null_mut());
                crate::cabi::zz_array_push(ptr, CValue::int(master as i64));
                crate::cabi::zz_array_push(ptr, CValue::int(slave as i64));
                crate::cabi::zz_variant_ok(arr)
            }
        }
        Err(msg) => err_wrap(&msg),
    }
}

/// `pty.read(fd, n) -> Result<str, str>` (lossy UTF-8).
#[no_mangle]
pub extern "C" fn zz_pty_read(fd: CValue, n: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let fd = fd.as_i64().unwrap_or(-1) as i32;
    let n = n.as_i64().unwrap_or(0) as usize;
    match pty_read(fd, n) {
        Ok(data) => ok_wrap(cvalue_str(data.as_bytes())),
        Err(msg) => err_wrap(&msg),
    }
}

/// `pty.write(fd, data) -> Result<int, str>` (bytes written).
#[no_mangle]
pub extern "C" fn zz_pty_write(fd: CValue, data: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let fd = fd.as_i64().unwrap_or(-1) as i32;
    let bytes = match cvalue_to_string(data) {
        Some(s) => s.into_bytes(),
        None => return err_wrap("pty.write: want str"),
    };
    match pty_write(fd, &bytes) {
        Ok(count) => ok_wrap(CValue::int(count)),
        Err(msg) => err_wrap(&msg),
    }
}

/// `pty.ptsname(fd) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_pty_ptsname(fd: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let fd = fd.as_i64().unwrap_or(-1) as i32;
    match pty_ptsname(fd) {
        Ok(path) => ok_wrap(cvalue_str(path.as_bytes())),
        Err(msg) => err_wrap(&msg),
    }
}

/// `pty.grantpt(fd) -> Result<unit, str>`.
#[no_mangle]
pub extern "C" fn zz_pty_grantpt(fd: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let fd = fd.as_i64().unwrap_or(-1) as i32;
    match pty_grantpt(fd) {
        Ok(()) => ok_wrap(CValue::unit()),
        Err(msg) => err_wrap(&msg),
    }
}

/// `pty.unlockpt(fd) -> Result<unit, str>`.
#[no_mangle]
pub extern "C" fn zz_pty_unlockpt(fd: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let fd = fd.as_i64().unwrap_or(-1) as i32;
    match pty_unlockpt(fd) {
        Ok(()) => ok_wrap(CValue::unit()),
        Err(msg) => err_wrap(&msg),
    }
}

/// `pty.set_winsz(fd, cols, rows) -> Result<unit, str>`.
#[no_mangle]
pub extern "C" fn zz_pty_set_winsz(
    fd: CValue,
    cols: CValue,
    rows: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let _ = err;
    let fd = fd.as_i64().unwrap_or(-1) as i32;
    let cols = cols.as_i64().unwrap_or(0) as i32;
    let rows = rows.as_i64().unwrap_or(0) as i32;
    match pty_set_winsz(fd, cols, rows) {
        Ok(()) => ok_wrap(CValue::unit()),
        Err(msg) => err_wrap(&msg),
    }
}

/// `pty.isatty(fd) -> bool`.
#[no_mangle]
pub extern "C" fn zz_pty_isatty(fd: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let fd = fd.as_i64().unwrap_or(-1) as i32;
    CValue::boolean(pty_isatty(fd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_roundtrip() {
        let (master, slave_fd) = pty_open().expect("pty.open");
        assert!(master >= 0);
        assert!(slave_fd >= 0);
        assert!(pty_isatty(slave_fd));
        let _ = pty_grantpt(slave_fd);
        let _ = pty_unlockpt(slave_fd);
        let _ = pty_set_winsz(master, 80, 24);
        // Write to slave (output) -> readable on master.
        let wrote = pty_write(slave_fd, b"hello").expect("pty.write");
        assert_eq!(wrote, 5);
        let got = pty_read(master, 64).expect("pty.read");
        assert_eq!(got, "hello");
        // Close fds.
        let _ = unsafe { libc::close(master) };
        let _ = unsafe { libc::close(slave_fd) };
    }

    #[test]
    fn ptsname_returns_slave_path() {
        let (master, _slave) = pty_open().expect("pty.open");
        let named = pty_ptsname(master).expect("pty.ptsname");
        assert!(!named.is_empty());
        assert!(named.starts_with("/dev/"));
        let _ = unsafe { libc::close(master) };
        let _ = unsafe { libc::close(_slave) };
    }
}
