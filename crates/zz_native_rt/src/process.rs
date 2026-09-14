//! `std.process` subprocess control for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! through thin `zz_stdlib` adapters, while AOT binaries call the
//! `extern "C"` `zz_process_*` functions (same `zz_value` convention).
//!
//! Model:
//! - `run` blocks: spawn, collect piped stdout/stderr, return the exit
//!   status. No shell is ever involved — `cmd` plus an explicit argv
//!   array, so there is no injection surface.
//! - Results cross as `[status, stdout, stderr]` arrays wrapped in
//!   `Result` (`.err` only when the child cannot even start, e.g.
//!   unknown binary; a failing child is `.ok` with non-zero status).
//!   Stream bytes use lossy UTF-8 (documented; binary output keeps its
//!   shape minus invalid sequences).
//! - `spawn` returns a pool handle (tag `"process"`); `wait` takes the
//!   child out exactly once (double-wait errors).
//! - Environment is inherited; `run_with_env` overlays `K=V` entries.
//! - stdin is inherited (children read our stdin); stdout/stderr are
//!   always piped and collected.
//! - `exit` terminates the whole process immediately (no destructors,
//!   flush before calling if output matters).

use std::process::{Child, Command};
use std::sync::{Arc, Mutex};

use crate::cabi::{array_push_str, cvalue_str, cvalue_to_str_vec, cvalue_to_string, CValue};
use crate::{alloc, payload, Handle};

/// Pool tag for spawned children. Selects the `process.*` namespace for
/// handle-taking functions.
pub const TAG: &str = "process";

/// A not-yet-waited child (`None` after `wait` takes it).
pub type ChildSlot = Mutex<Option<Child>>;

fn triple(status: i32, stdout: &[u8], stderr: &[u8]) -> (i64, String, String) {
    (
        status as i64,
        String::from_utf8_lossy(stdout).into_owned(),
        String::from_utf8_lossy(stderr).into_owned(),
    )
}

/// Run `cmd` with `argv` and `extra_env` overlays, blocking for output.
pub fn run(
    cmd: &str,
    argv: &[String],
    extra_env: &[(String, String)],
) -> Result<(i64, String, String), String> {
    let mut command = Command::new(cmd);
    command.args(argv);
    for (k, v) in extra_env {
        command.env(k, v);
    }
    match command.output() {
        Ok(out) => Ok(triple(
            out.status.code().unwrap_or(-1),
            &out.stdout,
            &out.stderr,
        )),
        Err(e) => Err(format!("process.run: cannot start `{cmd}`: {e}")),
    }
}

/// Spawn `cmd` without waiting; returns the child handle.
pub fn spawn(cmd: &str, argv: &[String]) -> Result<Handle, String> {
    let mut command = Command::new(cmd);
    command.args(argv);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    match command.spawn() {
        Ok(child) => Ok(alloc(TAG, Arc::new(Mutex::new(Some(child))))),
        Err(e) => Err(format!("process.spawn: cannot start `{cmd}`: {e}")),
    }
}

/// Why `wait` could not produce output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitError {
    /// No pool entry for the id (never allocated or already dropped).
    UnknownHandle,
    /// The child was already taken by an earlier `wait`.
    AlreadyWaited,
    /// The OS reported failure while reaping.
    Reap(String),
}

impl std::fmt::Display for WaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaitError::UnknownHandle => write!(f, "process.wait: unknown child handle"),
            WaitError::AlreadyWaited => write!(f, "process.wait: child already waited"),
            WaitError::Reap(e) => write!(f, "process.wait: {e}"),
        }
    }
}

/// Wait for `id`, returning its output triple. Single-shot.
pub fn wait(id: u64) -> Result<(i64, String, String), WaitError> {
    let slot: Arc<ChildSlot> = payload(id, TAG).ok_or(WaitError::UnknownHandle)?;
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    match guard.take() {
        Some(child) => match child.wait_with_output() {
            Ok(out) => Ok(triple(
                out.status.code().unwrap_or(-1),
                &out.stdout,
                &out.stderr,
            )),
            Err(e) => Err(WaitError::Reap(e.to_string())),
        },
        None => Err(WaitError::AlreadyWaited),
    }
}

/// Current process id.
pub fn pid() -> i64 {
    std::process::id() as i64
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

/// Build the `[status, stdout, stderr]` array (linked constructors).
fn triple_value(status: i64, stdout: &str, stderr: &str) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe {
        let arr = crate::cabi::zz_array_new();
        crate::cabi::zz_array_push(
            arr.as_array_ptr().unwrap_or(std::ptr::null_mut()),
            CValue::int(status),
        );
        array_push_str(arr, stdout);
        array_push_str(arr, stderr);
        arr
    }
}

fn ok_wrap(v: CValue) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_ok(v) }
}

fn err_wrap(s: &str) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_err(cvalue_str(s.as_bytes())) }
}

/// `process.run(cmd: str, argv: [str]) -> Result<[int,str,str], str>`.
#[no_mangle]
pub extern "C" fn zz_process_run(cmd: CValue, argv: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (cvalue_to_string(cmd), cvalue_to_str_vec(argv)) {
        (Some(c), Some(a)) => match run(&c, &a, &[]) {
            Ok((st, out, err_s)) => ok_wrap(triple_value(st, &out, &err_s)),
            Err(e) => err_wrap(&e),
        },
        _ => {
            set_err(err);
            err_wrap("process.run: want (str, [str])")
        }
    }
}

/// `process.run_with_env(cmd: str, argv: [str], env: [str]) -> Result<…>`
/// (`env` entries are `K=V` strings; malformed entries are skipped).
#[no_mangle]
pub extern "C" fn zz_process_run_with_env(
    cmd: CValue,
    argv: CValue,
    env: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (
        cvalue_to_string(cmd),
        cvalue_to_str_vec(argv),
        cvalue_to_str_vec(env),
    ) {
        (Some(c), Some(a), Some(e)) => {
            let overlays: Vec<(String, String)> = e
                .iter()
                .filter_map(|s| {
                    s.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect();
            match run(&c, &a, &overlays) {
                Ok((st, out, err_s)) => ok_wrap(triple_value(st, &out, &err_s)),
                Err(e) => err_wrap(&e),
            }
        }
        _ => {
            set_err(err);
            err_wrap("process.run_with_env: want (str, [str], [str])")
        }
    }
}

/// `process.spawn(cmd: str, argv: [str]) -> Result<int, str>` (child id).
#[no_mangle]
pub extern "C" fn zz_process_spawn(cmd: CValue, argv: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (cvalue_to_string(cmd), cvalue_to_str_vec(argv)) {
        (Some(c), Some(a)) => match spawn(&c, &a) {
            Ok(h) => ok_wrap(CValue::int(h.id as i64)),
            Err(e) => err_wrap(&e),
        },
        _ => {
            set_err(err);
            err_wrap("process.spawn: want (str, [str])")
        }
    }
}

/// `process.wait(id: int) -> Result<[int,str,str], str>`.
#[no_mangle]
pub extern "C" fn zz_process_wait(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    match id.as_i64() {
        Some(n) if n > 0 => match wait(n as u64) {
            Ok((st, out, err_s)) => ok_wrap(triple_value(st, &out, &err_s)),
            Err(e) => {
                set_err(err);
                err_wrap(&e.to_string())
            }
        },
        _ => {
            set_err(err);
            err_wrap("process.wait: want int handle")
        }
    }
}

/// `process.exit(code: int)` (never returns).
#[no_mangle]
pub extern "C" fn zz_process_exit(code: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    std::process::exit(code.as_i64().unwrap_or(0) as i32);
}

/// `process.pid() -> int`.
#[no_mangle]
pub extern "C" fn zz_process_pid(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    CValue::int(pid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_echo_roundtrip() {
        let (st, out, err_s) = run("echo", &["hello".to_string()], &[]).expect("run echo");
        assert_eq!(st, 0);
        assert_eq!(out, "hello\n");
        assert_eq!(err_s, "");
    }

    #[test]
    fn run_failing_status() {
        let (st, _, _) = run("false", &[], &[]).expect("run false");
        assert_ne!(st, 0);
    }

    #[test]
    fn run_missing_binary_errors() {
        let e = run("zz-definitely-not-a-binary", &[], &[]);
        assert!(e.is_err(), "missing binary must err, not panic");
    }

    #[test]
    fn run_with_env_overlay() {
        let (st, out, _) = run(
            "sh",
            &["-c".to_string(), "echo $ZZ_TEST_VAR".to_string()],
            &[("ZZ_TEST_VAR".to_string(), "overlay-ok".to_string())],
        )
        .expect("run sh");
        assert_eq!(st, 0);
        assert_eq!(out.trim(), "overlay-ok");
    }

    #[test]
    fn spawn_wait_single_shot() {
        let h = spawn("echo", &["spawned".to_string()]).expect("spawn");
        assert_eq!(h.tag, TAG);
        let (st, out, _) = wait(h.id).expect("wait ok");
        assert_eq!((st, out.as_str()), (0, "spawned\n"));
        // Second wait errors (child already taken).
        assert_eq!(wait(h.id).unwrap_err(), WaitError::AlreadyWaited);
        crate::drop_handle(h.id);
    }

    #[test]
    fn wait_unknown_handle() {
        assert_eq!(wait(999_999_999).unwrap_err(), WaitError::UnknownHandle);
    }

    #[test]
    fn pid_sane() {
        assert!(pid() > 0);
        assert_eq!(pid() as u32, std::process::id());
    }
}
