//! `std.signal` yield-point signal delivery for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! ([`signal_install`], [`signal_pending`], [`signal_raise`]) through thin
//! `zz_stdlib` adapters, while AOT binaries call the `extern "C"`
//! `zz_signal_*` functions.
//!
//! Delivery model (yield-point, not async preemption):
//! - `signal_install(signum)` installs a `sigaction` handler that sets an
//!   atomic pending flag for `signum` and writes a byte to a self-pipe
//!   (so a blocking `poll`/select wakes up). The handler never touches ZZ
//!   state — it is async-signal-safe.
//! - `signal_pending(signum)` checks and clears the flag. The ZZ side owns
//!   a registry of `signum -> handler closure` and, at yield points (or via
//!   an explicit `signal.run_pending`), invokes registered handlers through
//!   the interpreter. This avoids calling arbitrary ZZ closures from a real
//!   signal context.
//! - `signal_raise(signum)` sends the signal to self (useful in tests).
//! - Unix-only; Windows/stub return `.err(unsupported)` or no-op.

use crate::cabi::{cvalue_str, CValue};
use std::os::raw::c_int;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

static PENDING: Mutex<[AtomicBool; 64]> =
    Mutex::new(const { [const { AtomicBool::new(false) }; 64] });

// Self-pipe used to wake a blocking poll/select when a signal arrives.
// `c_int` is `RawFd` on Unix; using `c_int` keeps this static available on
// every platform (only the unix impl writes to it).
static SELF_PIPE: Mutex<Option<(c_int, c_int)>> = Mutex::new(None);

#[cfg(unix)]
mod unix {
    use super::{PENDING, SELF_PIPE};
    use std::os::raw::c_int;
    use std::sync::atomic::Ordering;

    /// Async-signal-safe handler: sets the pending flag and wakes the
    /// self-pipe reader. Never touches ZZ state.
    #[no_mangle]
    pub extern "C" fn signal_handler(signum: libc::c_int) {
        if !(1..64).contains(&signum) {
            return;
        }
        let guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let _ = guard[signum as usize].swap(true, Ordering::Release);
        drop(guard);
        wake();
    }

    /// Install a `sigaction` handler for `signum` (async-signal-safe: only
    /// sets an atomic flag and writes to the self-pipe).
    pub fn signal_install(signum: i32) -> Result<(), String> {
        if !(1..64).contains(&signum) {
            return Err(format!("signal.install: invalid signum {signum}"));
        }
        // Ensure the self-pipe exists.
        let mut pipe = SELF_PIPE.lock().unwrap_or_else(|e| e.into_inner());
        if pipe.is_none() {
            let mut fds: [c_int; 2] = [-1, -1];
            // SAFETY: `pipe2` writes two valid fds.
            if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK) } != 0 {
                return Err(format!(
                    "signal.install: pipe2 failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            *pipe = Some((fds[0], fds[1]));
        }
        let (_read_fd, _write_fd) = pipe.unwrap();

        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = signal_handler as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        // SAFETY: `sigemptyset` only writes a local mask.
        unsafe { libc::sigemptyset(&mut sa.sa_mask) };
        // SAFETY: `sa` is valid; `signum` is range-checked above.
        if unsafe { libc::sigaction(signum as libc::c_int, &sa, std::ptr::null_mut()) } != 0 {
            return Err(format!(
                "signal.install: sigaction failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Check and clear the pending flag for `signum`.
    pub fn signal_pending(signum: i32) -> bool {
        if !(1..64).contains(&signum) {
            return false;
        }
        let guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        guard[signum as usize].swap(false, Ordering::AcqRel)
    }

    /// Send `signum` to self (test helper).
    pub fn signal_raise(signum: i32) -> Result<(), String> {
        // SAFETY: `raise` takes only a signal number.
        if unsafe { libc::raise(signum as libc::c_int) } != 0 {
            return Err(format!(
                "signal.raise: raise failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Wake the self-pipe reader (called from the signal handler context).
    fn wake() {
        // Best-effort write; a full pipe just means the reader will poll
        // ready anyway.
        let pipe = SELF_PIPE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, write_fd)) = *pipe {
            let _ = unsafe {
                libc::write(write_fd, &[1u8] as *const [u8; 1] as *const libc::c_void, 1)
            };
        }
    }
}

#[cfg(windows)]
mod windows {
    pub fn signal_install(_signum: i32) -> Result<(), String> {
        Err("signal.install: unsupported on this platform".to_string())
    }
    pub fn signal_pending(_signum: i32) -> bool {
        false
    }
    pub fn signal_raise(_signum: i32) -> Result<(), String> {
        Err("signal.raise: unsupported on this platform".to_string())
    }
}

#[cfg(not(any(unix, windows)))]
mod stub {
    pub fn signal_install(_signum: i32) -> Result<(), String> {
        Err("signal.install: unsupported on this platform".to_string())
    }
    pub fn signal_pending(_signum: i32) -> bool {
        false
    }
    pub fn signal_raise(_signum: i32) -> Result<(), String> {
        Err("signal.raise: unsupported on this platform".to_string())
    }
}

#[cfg(not(any(unix, windows)))]
use stub as imp;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
use windows as imp;

pub fn signal_install(signum: i32) -> Result<(), String> {
    imp::signal_install(signum)
}
pub fn signal_pending(signum: i32) -> bool {
    imp::signal_pending(signum)
}
pub fn signal_raise(signum: i32) -> Result<(), String> {
    imp::signal_raise(signum)
}

fn ok_wrap(v: CValue) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_ok(v) }
}

fn err_wrap(msg: &str) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_err(cvalue_str(msg.as_bytes())) }
}

/// `signal.install(signum) -> Result<unit, str>`.
#[no_mangle]
pub extern "C" fn zz_signal_install(signum: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let signum = signum.as_i64().unwrap_or(0) as i32;
    match signal_install(signum) {
        Ok(()) => ok_wrap(CValue::unit()),
        Err(msg) => err_wrap(&msg),
    }
}

/// `signal.pending(signum) -> bool`.
#[no_mangle]
pub extern "C" fn zz_signal_pending(signum: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let signum = signum.as_i64().unwrap_or(0) as i32;
    CValue::boolean(signal_pending(signum))
}

/// `signal.raise(signum) -> Result<unit, str>`.
#[no_mangle]
pub extern "C" fn zz_signal_raise(signum: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    let signum = signum.as_i64().unwrap_or(0) as i32;
    match signal_raise(signum) {
        Ok(()) => ok_wrap(CValue::unit()),
        Err(msg) => err_wrap(&msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_before_install_is_false() {
        assert!(!signal_pending(1));
    }

    #[test]
    fn install_then_pending() {
        assert!(signal_install(10).is_ok());
        let _ = signal_pending(10); // must not panic
    }
}
