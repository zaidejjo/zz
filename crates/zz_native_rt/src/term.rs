//! `std.term` terminal control for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! ([`enable_raw`], [`disable_raw`], [`read_key`], [`get_size`],
//! [`is_tty`]) through thin `zz_stdlib` adapters, while AOT binaries call
//! the `extern "C"` `zz_term_*` functions (same `zz_value` convention as
//! `zz_sys_*` / `zz_regexp_*`).
//!
//! Model:
//! - `enable_raw` saves the current `termios` for stdin once (idempotent)
//!   and switches stdin to raw: `ICANON`/`ECHO` off (byte-at-a-time,
//!   no echo), `ISIG` off (Ctrl+C arrives as byte `3` so ZZ code can
//!   restore the terminal via `defer` instead of dying with a raw TTY),
//!   `IEXTEN` off, `IXON`/`ICRNL` off (no flow-control or CR mangling),
//!   `OPOST` off, `CS8`, `VMIN=1`/`VTIME=0` (blocking single-byte reads).
//! - `disable_raw` restores the saved state (idempotent — a second call
//!   is a no-op success).
//! - `read_key` blocks for exactly one byte on stdin and returns it as
//!   `0–255`. Multi-byte keys (arrows: `27, 91, 68/67/65/66`) decode
//!   ZZ-side with successive calls. `EINTR` is retried; EOF/closed stdin
//!   is an error, never a hang.
//! - Non-TTY (pipes, CI, redirected stdin) fails soft with
//!   `.err("std.term.<op>: not a tty")` — callers degrade to `input()`.
//! - Windows is a stub: same shapes, fallible ops `.err(unsupported)`
//!   (`disable_raw`/`is_tty` stay total-friendly: no-op `Ok` / `false`).

use crate::cabi::{cvalue_str, CValue};

#[cfg(unix)]
mod unix {
    use std::sync::Mutex;

    /// Saved pre-raw `termios` for stdin (`None` = not currently raw).
    static SAVED: Mutex<Option<libc::termios>> = Mutex::new(None);

    fn lock_saved() -> std::sync::MutexGuard<'static, Option<libc::termios>> {
        SAVED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn is_tty() -> bool {
        // SAFETY: `isatty` takes a bare fd, no pointers, no state.
        unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
    }

    pub fn enable_raw() -> Result<(), String> {
        // The guard is held for the whole transition so two tasks racing
        // `enable_raw` cannot snapshot-and-clobber each other's state
        // (the loser would otherwise save an already-raw termios).
        let mut saved = lock_saved();
        if saved.is_some() {
            return Ok(());
        }
        if !is_tty() {
            return Err("std.term.enable_raw: not a tty".to_string());
        }
        // SAFETY: `orig` is a valid local; `tcgetattr` only writes it on
        // success (checked below).
        let mut orig: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: stdin fd + valid out-param.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut orig) } != 0 {
            return Err(format!(
                "std.term.enable_raw: tcgetattr failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut raw = orig;
        raw.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
        raw.c_oflag &= !libc::OPOST;
        raw.c_cflag |= libc::CS8;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: `raw` is a valid termios; `TCSAFLUSH` discards pending
        // line-buffered input so the first `read_key` sees fresh keys.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw) } != 0 {
            return Err(format!(
                "std.term.enable_raw: tcsetattr failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        *saved = Some(orig);
        Ok(())
    }

    pub fn disable_raw() -> Result<(), String> {
        // Guard held across take + restore: a concurrent `enable_raw`
        // cannot slip in between and lose the saved state.
        let mut saved = lock_saved();
        let Some(orig) = saved.take() else {
            return Ok(());
        };
        // SAFETY: `orig` came from a successful `tcgetattr` on this fd.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &orig) } != 0 {
            // Restore failed — put the snapshot back so a retry still
            // restores the true original instead of erroring as no-op.
            *saved = Some(orig);
            return Err(format!(
                "std.term.disable_raw: tcsetattr failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub fn read_key() -> Result<i64, String> {
        if !is_tty() {
            return Err("std.term.read_key: not a tty".to_string());
        }
        let mut byte = [0u8; 1];
        loop {
            // SAFETY: `byte` is a valid 1-byte buffer for the call.
            let n = unsafe { libc::read(libc::STDIN_FILENO, byte.as_mut_ptr() as *mut _, 1) };
            if n == 1 {
                return Ok(byte[0] as i64);
            }
            if n == 0 {
                return Err("std.term.read_key: stdin closed".to_string());
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(format!("std.term.read_key: read failed: {err}"));
        }
    }

    pub fn get_size() -> Result<(i64, i64), String> {
        // SAFETY: `ws` is a valid local; `ioctl` only writes it on
        // success (checked via the return value).
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        for fd in [libc::STDOUT_FILENO, libc::STDIN_FILENO, libc::STDERR_FILENO] {
            // SAFETY: fd + valid out-param + correct request code.
            if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0
                && ws.ws_col != 0
                && ws.ws_row != 0
            {
                return Ok((ws.ws_col as i64, ws.ws_row as i64));
            }
        }
        Err("std.term.get_size: not a tty".to_string())
    }
}

#[cfg(not(unix))]
mod unix {
    pub fn is_tty() -> bool {
        false
    }

    pub fn enable_raw() -> Result<(), String> {
        Err("std.term.enable_raw: unsupported on this platform".to_string())
    }

    pub fn disable_raw() -> Result<(), String> {
        // Disabling is always a no-op success when raw was never enabled
        // (idempotent cleanup, e.g. unconditional `defer`).
        Ok(())
    }

    pub fn read_key() -> Result<i64, String> {
        Err("std.term.read_key: unsupported on this platform".to_string())
    }

    pub fn get_size() -> Result<(i64, i64), String> {
        Err("std.term.get_size: unsupported on this platform".to_string())
    }
}

/// True when stdin is a TTY (total — never fails).
pub fn is_tty() -> bool {
    unix::is_tty()
}

/// Switch stdin to raw mode (idempotent). See module docs.
pub fn enable_raw() -> Result<(), String> {
    unix::enable_raw()
}

/// Restore the pre-raw terminal state (idempotent no-op when not raw).
pub fn disable_raw() -> Result<(), String> {
    unix::disable_raw()
}

/// Block for one stdin byte, returning `0–255`.
pub fn read_key() -> Result<i64, String> {
    unix::read_key()
}

/// Terminal `(cols, rows)` via `TIOCGWINSZ`.
pub fn get_size() -> Result<(i64, i64), String> {
    unix::get_size()
}

/// Flush stdout immediately (total — interactive renders without a
/// trailing newline would otherwise sit buffered while `read_key`
/// blocks; `println`/`input` flush on their own, `print` does not).
pub fn flush() {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    // AOT programs print through C stdio (`fputs`), which owns a separate
    // buffer from Rust's stdout in the same process: a Rust-only flush
    // leaves prompts stranded in the C buffer while `read_key` blocks
    // (invisible prompt on a TTY, deadlock-looking hang). Flushing all C
    // output streams closes the gap on both engines.
    //
    // SAFETY: `fflush(NULL)` flushes all C output streams; always safe.
    unsafe {
        libc::fflush(std::ptr::null_mut());
    }
}

fn ok_unit() -> CValue {
    CValue::unit()
}

fn ok_wrap(v: CValue) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_ok(v) }
}

fn err_wrap(msg: &str) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_err(cvalue_str(msg.as_bytes())) }
}

/// `term.enable_raw() -> Result<unit, str>`.
#[no_mangle]
pub extern "C" fn zz_term_enable_raw(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    match enable_raw() {
        Ok(()) => ok_wrap(ok_unit()),
        Err(msg) => err_wrap(&msg),
    }
}

/// `term.disable_raw() -> Result<unit, str>`.
#[no_mangle]
pub extern "C" fn zz_term_disable_raw(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    match disable_raw() {
        Ok(()) => ok_wrap(ok_unit()),
        Err(msg) => err_wrap(&msg),
    }
}

/// `term.read_key() -> Result<int, str>` (byte `0–255`).
#[no_mangle]
pub extern "C" fn zz_term_read_key(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    match read_key() {
        Ok(b) => ok_wrap(CValue::int(b)),
        Err(msg) => err_wrap(&msg),
    }
}

/// `term.get_size() -> Result<[int, int], str>` (`[cols, rows]`).
#[no_mangle]
pub extern "C" fn zz_term_get_size(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    match get_size() {
        Ok((cols, rows)) => {
            // SAFETY: constructors are provided by the linked AOT program.
            unsafe {
                let arr = crate::cabi::zz_array_new();
                crate::cabi::zz_array_push(
                    arr.as_array_ptr().unwrap_or(std::ptr::null_mut()),
                    CValue::int(cols),
                );
                crate::cabi::zz_array_push(
                    arr.as_array_ptr().unwrap_or(std::ptr::null_mut()),
                    CValue::int(rows),
                );
                crate::cabi::zz_variant_ok(arr)
            }
        }
        Err(msg) => err_wrap(&msg),
    }
}

/// `term.is_tty() -> bool` (total).
#[no_mangle]
pub extern "C" fn zz_term_is_tty(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    CValue::boolean(is_tty())
}

/// `term.flush()` (total).
#[no_mangle]
pub extern "C" fn zz_term_flush(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    flush();
    CValue::unit()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hermetic stdin cannot be assumed: `cargo test` inherits the
    /// developer's terminal, where `enable_raw` would mutate a real TTY
    /// and `read_key` would block forever. Skip the stdin-touching
    /// asserts there (CI always pipes stdin, so coverage holds).
    fn skip_if_tty() -> bool {
        if is_tty() {
            eprintln!("std.term tests: stdin is a tty, skipping hermetic asserts");
            true
        } else {
            false
        }
    }

    #[test]
    fn piped_stdin_reports_not_a_tty() {
        if skip_if_tty() {
            return;
        }
        // Test harness stdin is piped, never a TTY: every fallible op
        // must fail soft (never block, never touch real termios).
        assert!(!is_tty());
        assert!(enable_raw().is_err());
        assert!(get_size().is_err());
        assert!(read_key().is_err());
    }

    #[test]
    fn disable_without_enable_is_ok() {
        // Idempotent restore: safe to call unconditionally in cleanup.
        assert!(disable_raw().is_ok());
    }

    #[test]
    fn error_messages_are_namespaced() {
        if skip_if_tty() {
            return;
        }
        for msg in [
            enable_raw().unwrap_err(),
            get_size().unwrap_err(),
            read_key().unwrap_err(),
        ] {
            assert!(msg.starts_with("std.term."), "unexpected: {msg}");
        }
    }
}
