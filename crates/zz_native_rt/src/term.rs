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
//! - Windows: `is_tty` is real (`std::io::IsTerminal`) and `poll` follows
//!   the same contract as Unix for consoles, pipes and files (it never
//!   fails just because stdin is not a console). Raw mode / `read_key` /
//!   `get_size` remain explicit `.err(unsupported)` stubs; `disable_raw`
//!   stays a no-op `Ok`.

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

    /// True when stdin has a byte ready, waiting at most `ms`.
    /// `ms <= 0` polls without blocking. Works on pipes as well as TTYs
    /// (closed stdin reports ready — the subsequent read returns EOF).
    /// EINTR recomputes the deadline and retries; only real failures Err.
    pub fn poll(ms: i64) -> Result<bool, String> {
        use std::time::Instant;
        let timeout = ms.max(0);
        let deadline = Instant::now()
            .checked_add(std::time::Duration::from_millis(timeout as u64))
            .unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(86400));
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .map(|d| d.as_millis().min(i32::MAX as u128) as i32)
                .unwrap_or(0);
            let mut pfd = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is a valid 1-element array; timeout is ms.
            let n = unsafe { libc::poll(&mut pfd, 1, remaining) };
            if n > 0 {
                return Ok(true);
            }
            if n == 0 {
                // Timeout — unless the clock says otherwise (spurious
                // wakeup with time left: re-poll the remainder).
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                continue;
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                continue;
            }
            return Err(format!("std.term.poll: poll failed: {err}"));
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

#[cfg(windows)]
mod windows {
    //! Console/pipes/files share one `poll` contract with Unix: it answers
    //! "is stdin readable within `ms`?" and never fails just because stdin
    //! is not a console. `cargo test` on CI redirects stdin, so a
    //! console-only implementation that returns `Err` would fail for a
    //! reason the API does not promise.
    //!
    //! No `windows-sys` dependency: these kernel32 imports are stable ABI,
    //! and the crate already links kernel32 through std.
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const FILE_TYPE_DISK: u32 = 0x0001;
    const FILE_TYPE_PIPE: u32 = 0x0003;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

    #[link(name = "kernel32")]
    #[allow(non_snake_case)] // Win32 entry points keep their documented names.
    extern "system" {
        fn GetStdHandle(nstdhandle: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn GetNumberOfConsoleInputEvents(handle: *mut c_void, count: *mut u32) -> i32;
        fn GetFileType(handle: *mut c_void) -> u32;
        fn PeekNamedPipe(
            pipe: *mut c_void,
            buffer: *mut c_void,
            buffer_size: u32,
            bytes_read: *mut u32,
            bytes_available: *mut u32,
            bytes_left: *mut u32,
        ) -> i32;
    }

    pub fn is_tty() -> bool {
        use std::io::IsTerminal as _;
        std::io::stdin().is_terminal()
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

    /// Like Unix `poll(POLLIN)`: a console with queued input events, a pipe
    /// with buffered bytes (or a closed writer → EOF is readable), and a
    /// regular file are ready; everything else (NUL, unknown handles)
    /// reports "no input" instead of failing.
    fn input_ready() -> bool {
        // SAFETY: every call takes the value returned by `GetStdHandle` and
        // writes only to valid stack out-params. A null/invalid handle is
        // rejected up front; failure of any probe degrades to "not ready".
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut mode = 0u32;
            if GetConsoleMode(handle, &mut mode) != 0 {
                let mut events = 0u32;
                return GetNumberOfConsoleInputEvents(handle, &mut events) != 0 && events > 0;
            }
            match GetFileType(handle) {
                // Regular files never block, matching Unix poll semantics.
                FILE_TYPE_DISK => true,
                FILE_TYPE_PIPE => {
                    let mut available = 0u32;
                    let ok = PeekNamedPipe(
                        handle,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    );
                    // Peek fails once the writer is gone (ERROR_BROKEN_PIPE):
                    // the read side is readable (EOF), same as Linux POLLHUP.
                    ok == 0 || available > 0
                }
                _ => false,
            }
        }
    }

    pub fn poll(ms: i64) -> Result<bool, String> {
        let timeout = Duration::from_millis(ms.max(0) as u64);
        let deadline = Instant::now() + timeout;
        loop {
            if input_ready() {
                return Ok(true);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            // Windows has no waitable "console input has events" primitive
            // that also covers pipes/files, so re-probe on a short tick.
            std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
        }
    }

    pub fn get_size() -> Result<(i64, i64), String> {
        Err("std.term.get_size: unsupported on this platform".to_string())
    }
}

#[cfg(not(any(unix, windows)))]
mod stub {
    pub fn is_tty() -> bool {
        false
    }

    pub fn enable_raw() -> Result<(), String> {
        Err("std.term.enable_raw: unsupported on this platform".to_string())
    }

    pub fn disable_raw() -> Result<(), String> {
        Ok(())
    }

    pub fn read_key() -> Result<i64, String> {
        Err("std.term.read_key: unsupported on this platform".to_string())
    }

    pub fn poll(_ms: i64) -> Result<bool, String> {
        Err("std.term.poll: unsupported on this platform".to_string())
    }

    pub fn get_size() -> Result<(i64, i64), String> {
        Err("std.term.get_size: unsupported on this platform".to_string())
    }
}

#[cfg(not(any(unix, windows)))]
use stub as imp;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
use windows as imp;

/// True when stdin is a TTY (total — never fails).
pub fn is_tty() -> bool {
    imp::is_tty()
}

/// Switch stdin to raw mode (idempotent). See module docs.
pub fn enable_raw() -> Result<(), String> {
    imp::enable_raw()
}

/// Restore the pre-raw terminal state (idempotent no-op when not raw).
pub fn disable_raw() -> Result<(), String> {
    imp::disable_raw()
}

/// Block for one stdin byte, returning `0–255`.
pub fn read_key() -> Result<i64, String> {
    imp::read_key()
}

/// True when stdin has a byte ready within `ms` (`ms <= 0` = no wait).
pub fn poll(ms: i64) -> Result<bool, String> {
    imp::poll(ms)
}

/// Terminal `(cols, rows)` via `TIOCGWINSZ`.
pub fn get_size() -> Result<(i64, i64), String> {
    imp::get_size()
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

/// `term.poll(ms) -> Result<bool, str>` (input ready within `ms`).
#[no_mangle]
pub extern "C" fn zz_term_poll(ms: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let ms = ms.as_i64().unwrap_or(0);
    match poll(ms) {
        Ok(ready) => ok_wrap(CValue::boolean(ready)),
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
    fn poll_zero_returns_immediately() {
        // `poll(0)` never blocks: on piped CI stdin it resolves at once
        // (ready on EOF, not-ready on an idle pipe — either is `Ok`).
        // This also bounds the test's wall time by construction.
        let start = std::time::Instant::now();
        let r = poll(0);
        assert!(r.is_ok(), "poll(0) failed: {r:?}");
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn poll_negative_clamps_to_zero() {
        let start = std::time::Instant::now();
        let r = poll(-100);
        assert!(r.is_ok(), "poll(-100) failed: {r:?}");
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
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
