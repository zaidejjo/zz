//! `std.fs` — comprehensive, non-blocking filesystem module.
//!
//! # Architecture
//!
//! Every operation looks synchronous to ZZ code (`fs.read_to_string(path)`
//! returns its `Result` directly) but never blocks a green-thread executor
//! thread. When called on an executor thread with no interpreter frames above
//! the native (`on_executor() && interp_depth() == 0`), the blocking
//! syscall is submitted to a dedicated I/O worker pool and the task yields
//! via the existing channel-wait protocol (`YieldReason::ChanWait`): the
//! executor parks the task and resumes it when the I/O thread hands the
//! result back through a one-shot channel. Main-thread calls and nested
//! interpreter calls run the syscall inline (topping up the executor when
//! nested so scheduler throughput never collapses).
//!
//! # Errors
//!
//! All failures surface as `.err(str)` with a unified, platform-independent
//! shape — `fs:<op>:<code>: <path>` — instead of raw OS `errno` text, so the
//! VM and the AOT C runtime produce identical diagnostics. `<code>` is one
//! of `not_found`, `permission_denied`, `already_exists`, `invalid_input`,
//! `not_empty`, `closed`, or `io_error`.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Condvar, Mutex, OnceLock,
};
use std::time::UNIX_EPOCH;

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::expect_str;

// ── Unified error codes ────────────────────────────────────────────────────

/// Map an I/O error to a stable, platform-independent code.
fn fs_code(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => "not_found",
        K::PermissionDenied => "permission_denied",
        K::AlreadyExists => "already_exists",
        K::InvalidInput | K::InvalidData | K::UnexpectedEof => "invalid_input",
        K::DirectoryNotEmpty => "not_empty",
        // BrokenPipe / Connection* never occur for fs; الجديد IsADirectory /
        // NotADirectory (stable since 1.83 as `IsADirectory`/`NotADirectory`)
        // fall through to `io_error` on older toolchains via the catch-all.
        _ => {
            let msg = e.to_string();
            // `remove_dir_all` on a non-empty dir and friends surface here on
            // some platforms; keep the raw-code contract stable anyway.
            if msg.contains("Directory not empty") {
                "not_empty"
            } else {
                "io_error"
            }
        }
    }
}

/// Build the unified `.err` payload: `fs:<op>:<code>: <path>`.
fn fs_err(op: &str, path: &str, e: &std::io::Error) -> Value {
    err_str(format!("fs:{op}:{}: {path}", fs_code(e)))
}

/// Unified error for two-path ops: `fs:<op>:<code>: <src> -> <dst>`.
fn fs_err2(op: &str, src: &str, dst: &str, e: &std::io::Error) -> Value {
    err_str(format!("fs:{op}:{}: {src} -> {dst}", fs_code(e)))
}

fn ok_unit() -> Value {
    Value::Result(Box::new(Ok(Value::Unit)))
}

fn ok_str(s: String) -> Value {
    Value::Result(Box::new(Ok(Value::Str(Box::new(s)))))
}

fn err_str(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(Box::new(msg)))))
}

fn ok_value(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

// ── I/O worker pool ────────────────────────────────────────────────────────
///
/// Fixed pool of OS threads (one per CPU, capped at 8) draining a shared job
/// queue. Blocking syscalls run here; green tasks yield while they do. Zero
/// third-party dependencies — plain `std::sync::mpsc` + `std::thread`.
type Job = Box<dyn FnOnce() + Send + 'static>;

struct FsPool {
    tx: std::sync::mpsc::Sender<Job>,
}

impl FsPool {
    fn global() -> &'static FsPool {
        static POOL: OnceLock<FsPool> = OnceLock::new();
        POOL.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Job>();
            let rx = Arc::new(Mutex::new(rx));
            let n = std::thread::available_parallelism()
                .map(|n| n.get().clamp(2, 8))
                .unwrap_or(4);
            for _ in 0..n {
                let rx = Arc::clone(&rx);
                std::thread::Builder::new()
                    .name("zz-fs-io".into())
                    .spawn(move || loop {
                        let job = rx.lock().unwrap().recv();
                        match job {
                            Ok(j) => j(),
                            Err(_) => break, // sender dropped: shut down
                        }
                    })
                    .expect("zz-fs-io thread spawn failed");
            }
            FsPool { tx }
        })
    }

    fn submit(job: Job) {
        // The pool lives for the process; a failed send means every worker
        // is gone (only via panics, which are caught per-job below) — run
        // inline so the caller never hangs on a lost job.
        if let Err(std::sync::mpsc::SendError(job)) = Self::global().tx.send(job) {
            job();
        }
    }
}

// ── Green / nested detection ───────────────────────────────────────────────

fn is_green_task() -> bool {
    zz_runtime::value::on_executor() && zz_runtime::value::interp_depth() == 0
}

// ── One-shot channel bridge ────────────────────────────────────────────────
//
// The I/O thread hands the computed `Value` back through a fresh channel;
// the task side reuses the tested `chan.recv` path (fast value or
// `ChanWait` yield). The low-level send below mirrors `chan.send` without
// needing an `Interp` on the I/O thread.

fn fresh_chan() -> Arc<zz_runtime::value::ChanState> {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    Arc::new(zz_runtime::value::ChanState {
        ring: zz_runtime::lf_chan::LfRing::new(),
        spill: AtomicUsize::new(0),
        green_parked: AtomicBool::new(false),
        cvar_waiters: AtomicUsize::new(0),
        inner: Mutex::new(zz_runtime::value::ChanInner {
            queue: std::collections::VecDeque::new(),
            green_waiters: Vec::new(),
        }),
        cvar: Condvar::new(),
    })
}

fn chan_send_from_io(state: &Arc<zz_runtime::value::ChanState>, v: Value) {
    use std::sync::atomic::Ordering;
    if let Err(back) = state.ring.try_enqueue(v) {
        let guard = state.inner.lock().unwrap();
        // `ChanInner.queue` is a VecDeque in this workspace version.
        let mut guard = guard;
        guard.queue.push_back(back);
        state.spill.fetch_add(1, Ordering::Release);
        super::concurrency::service_chan_waiters(state, guard, Span::new(0, 0));
        return;
    }
    let guard = state.inner.lock().unwrap();
    super::concurrency::service_chan_waiters(state, guard, Span::new(0, 0));
}

/// Run `op` to a `Value` without blocking the scheduler.
///
/// - Green task: submit to the I/O pool, then `chan.recv` the one-shot
///   (yields the task until the I/O thread delivers).
/// - Nested on an executor thread: top up a replacement, run inline.
/// - Main thread: run inline.
fn run_fs(
    interp: &mut Interp,
    span: Span,
    op: impl FnOnce() -> Value + Send + 'static,
) -> Result<Value, EvalError> {
    if is_green_task() {
        let state = fresh_chan();
        let tx_state = Arc::clone(&state);
        FsPool::submit(Box::new(move || {
            // A panicking op must never strand the parked task: resume it
            // with a loud `io_error` instead of hanging the scheduler.
            let v =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(op)).unwrap_or_else(|_| {
                    err_str("fs:io_error: worker panic during file operation".to_string())
                });
            chan_send_from_io(&tx_state, v);
        }));
        let mut args = vec![Value::Chan(state)];
        return super::concurrency::chan_recv(interp, &mut args, span);
    }
    if zz_runtime::value::on_executor() {
        super::concurrency::executor::Executor::top_up();
    }
    Ok(op())
}

// ── Streaming file handles ─────────────────────────────────────────────────
//
// Open handles live in a process-wide pool under tag `"file"` (selecting the
// `file.*` method namespace). The pool owns an `Arc<FsFile>` each; `close`
// shuts the OS file and removes the entry so later uses report `closed`.

/// Pool tag for open file handles. Selects the `file.*` method namespace.
pub(crate) const FILE_TAG: &str = "file";

/// An open streaming file: the OS handle plus its origin path.
pub(crate) struct FsFile {
    file: Mutex<Option<std::fs::File>>,
    path: String,
}

static FILE_POOL: OnceLock<Mutex<HashMap<u64, Arc<FsFile>>>> = OnceLock::new();
static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(1);

fn file_pool() -> &'static Mutex<HashMap<u64, Arc<FsFile>>> {
    FILE_POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_alloc(file: std::fs::File, path: String) -> u64 {
    let id = NEXT_FILE_ID.fetch_add(1, Ordering::SeqCst);
    assert!(id != 0, "fs file id counter exhausted");
    file_pool().lock().unwrap().insert(
        id,
        Arc::new(FsFile {
            file: Mutex::new(Some(file)),
            path,
        }),
    );
    id
}

fn file_lookup(id: u64) -> Option<Arc<FsFile>> {
    file_pool().lock().unwrap().get(&id).cloned()
}

fn file_drop(id: u64) -> bool {
    file_pool().lock().unwrap().remove(&id).is_some()
}

fn expect_file(
    args: &mut Vec<Value>,
    i: usize,
    op: &str,
    name: &str,
) -> Result<Result<(u64, Arc<FsFile>), Value>, EvalError> {
    let handle = match args.get(i) {
        Some(Value::Opaque(h)) if h.tag == FILE_TAG => h.clone(),
        Some(other) => {
            return Err(EvalError::new(
                format!("`{name}` expects a file handle, found `{other}`"),
                Span::new(0, 0),
            ));
        }
        None => {
            return Err(EvalError::new(
                format!("missing argument for `{name}`"),
                Span::new(0, 0),
            ));
        }
    };
    match file_lookup(handle.id) {
        Some(f) => Ok(Ok((handle.id, f))),
        // Unknown or already-closed ids are soft unified `.err` values
        // (`fs:<op>:closed`, no handle ids — ids differ across engines and
        // must never leak into compared output), never hard errors.
        None => Ok(Err(err_str(format!("fs:{op}:closed")))),
    }
}

fn file_value(id: u64) -> Value {
    ok_value(Value::Opaque(Box::new(zz_native_rt::Handle {
        tag: FILE_TAG.to_string(),
        id,
    })))
}

// ── Basic operations ───────────────────────────────────────────────────────

pub(crate) fn fs_read_file(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.read_file")?;
    run_fs(interp, span, move || match std::fs::read_to_string(&path) {
        Ok(contents) => ok_str(contents),
        Err(e) => fs_err("read", &path, &e),
    })
}

pub(crate) fn fs_read_bytes(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.read_bytes")?;
    run_fs(interp, span, move || match std::fs::read(&path) {
        Ok(bytes) => ok_value(Value::Array(Box::new(
            bytes.into_iter().map(|b| Value::Int(b as i64)).collect(),
        ))),
        Err(e) => fs_err("read_bytes", &path, &e),
    })
}

pub(crate) fn fs_write_file(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.write_file")?;
    let contents = expect_str(args, 1, "std.fs.write_file")?;
    run_fs(interp, span, move || {
        match std::fs::write(&path, contents) {
            Ok(()) => ok_unit(),
            Err(e) => fs_err("write", &path, &e),
        }
    })
}

pub(crate) fn fs_append(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.append")?;
    let contents = expect_str(args, 1, "std.fs.append")?;
    run_fs(interp, span, move || {
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(contents.as_bytes()));
        match res {
            Ok(()) => ok_unit(),
            Err(e) => fs_err("append", &path, &e),
        }
    })
}

pub(crate) fn fs_copy(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let src = expect_str(args, 0, "std.fs.copy")?;
    let dst = expect_str(args, 1, "std.fs.copy")?;
    run_fs(interp, span, move || match std::fs::copy(&src, &dst) {
        Ok(_) => ok_unit(),
        Err(e) => fs_err2("copy", &src, &dst, &e),
    })
}

pub(crate) fn fs_move(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let src = expect_str(args, 0, "std.fs.move")?;
    let dst = expect_str(args, 1, "std.fs.move")?;
    run_fs(interp, span, move || match std::fs::rename(&src, &dst) {
        Ok(()) => ok_unit(),
        Err(e) => fs_err2("move", &src, &dst, &e),
    })
}

pub(crate) fn fs_exists(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.exists")?;
    // Metadata probe: no pool hop needed (single stat syscall, no content).
    Ok(Value::Bool(std::path::Path::new(&path).exists()))
}

pub(crate) fn fs_is_file(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.is_file")?;
    Ok(Value::Bool(
        std::fs::metadata(&path).is_ok_and(|m| m.is_file()),
    ))
}

pub(crate) fn fs_is_dir(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.is_dir")?;
    Ok(Value::Bool(
        std::fs::metadata(&path).is_ok_and(|m| m.is_dir()),
    ))
}

pub(crate) fn fs_remove_file(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.remove_file")?;
    run_fs(interp, span, move || match std::fs::remove_file(&path) {
        Ok(()) => ok_unit(),
        Err(e) => fs_err("remove_file", &path, &e),
    })
}

// ── Directory management ───────────────────────────────────────────────────

pub(crate) fn fs_mkdir(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.mkdir")?;
    run_fs(interp, span, move || match std::fs::create_dir(&path) {
        Ok(()) => ok_unit(),
        Err(e) => fs_err("mkdir", &path, &e),
    })
}

pub(crate) fn fs_mkdir_all(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.mkdir_all")?;
    run_fs(interp, span, move || match std::fs::create_dir_all(&path) {
        Ok(()) => ok_unit(),
        Err(e) => fs_err("mkdir_all", &path, &e),
    })
}

fn sorted_dir_entries(path: &str) -> Result<Vec<String>, std::io::Error> {
    let mut out: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push(name);
    }
    out.sort();
    Ok(out)
}

pub(crate) fn fs_read_dir(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.read_dir")?;
    run_fs(interp, span, move || match sorted_dir_entries(&path) {
        Ok(names) => ok_value(Value::Array(Box::new(
            names.into_iter().map(|n| Value::Str(Box::new(n))).collect(),
        ))),
        Err(e) => fs_err("read_dir", &path, &e),
    })
}

pub(crate) fn fs_remove_dir_all(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.remove_dir_all")?;
    run_fs(interp, span, move || match std::fs::remove_dir_all(&path) {
        Ok(()) => ok_unit(),
        Err(e) => fs_err("remove_dir_all", &path, &e),
    })
}

fn walk_sorted(root: &str) -> Result<Vec<String>, std::io::Error> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_string()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<(String, String)> = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let p = entry.path();
            let s = p.to_string_lossy().into_owned();
            let is_dir = entry.file_type()?.is_dir();
            entries.push((s, if is_dir { "d" } else { "f" }.to_string()));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        // Push dirs in reverse so the pop order stays sorted; record every
        // path (files and dirs) in sorted order.
        for (s, kind) in entries.iter().rev() {
            if kind == "d" {
                stack.push(s.clone());
            }
        }
        for (s, _) in entries {
            out.push(s);
        }
    }
    out.sort();
    Ok(out)
}

pub(crate) fn fs_walk_dir(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.walk_dir")?;
    run_fs(interp, span, move || match walk_sorted(&path) {
        Ok(paths) => ok_value(Value::Array(Box::new(
            paths.into_iter().map(|p| Value::Str(Box::new(p))).collect(),
        ))),
        Err(e) => fs_err("walk_dir", &path, &e),
    })
}

// ── Metadata ───────────────────────────────────────────────────────────────

fn ms_since_epoch(t: std::time::SystemTime) -> String {
    t.duration_since(UNIX_EPOCH)
        .map(|d| (d.as_millis()).to_string())
        .unwrap_or_else(|_| "0".to_string())
}

pub(crate) fn fs_stat(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.stat")?;
    run_fs(interp, span, move || match std::fs::metadata(&path) {
        Ok(m) => {
            let modified = m
                .modified()
                .map(ms_since_epoch)
                .unwrap_or_else(|_| "0".into());
            let created = m
                .created()
                .map(ms_since_epoch)
                .unwrap_or_else(|_| "0".into());
            let pairs = vec![
                (
                    Value::Str(Box::new("size".to_string())),
                    Value::Str(Box::new(m.len().to_string())),
                ),
                (
                    Value::Str(Box::new("modified_ms".to_string())),
                    Value::Str(Box::new(modified)),
                ),
                (
                    Value::Str(Box::new("created_ms".to_string())),
                    Value::Str(Box::new(created)),
                ),
                (
                    Value::Str(Box::new("is_file".to_string())),
                    Value::Str(Box::new(m.is_file().to_string())),
                ),
                (
                    Value::Str(Box::new("is_dir".to_string())),
                    Value::Str(Box::new(m.is_dir().to_string())),
                ),
                (
                    Value::Str(Box::new("readonly".to_string())),
                    Value::Str(Box::new(m.permissions().readonly().to_string())),
                ),
            ];
            ok_value(Value::Dict(Box::new(pairs)))
        }
        Err(e) => fs_err("stat", &path, &e),
    })
}

// ── Streaming handles ──────────────────────────────────────────────────────

pub(crate) fn fs_open(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.open")?;
    let mode = expect_str(args, 1, "std.fs.open")?;
    if !matches!(mode.as_str(), "r" | "w" | "a") {
        return Ok(err_str(format!(
            "fs:open:invalid_input: {path} (mode must be one of r, w, a)"
        )));
    }
    run_fs(interp, span, move || {
        let opened = match mode.as_str() {
            "r" => std::fs::File::open(&path),
            "w" => std::fs::File::create(&path),
            _ => std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path),
        };
        match opened {
            Ok(f) => file_value(file_alloc(f, path)),
            Err(e) => fs_err("open", &path, &e),
        }
    })
}

pub(crate) fn file_read_chunk(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_file(args, 0, "read_chunk", "file.read_chunk")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let n: usize = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => (*n as usize).min(8 * 1024 * 1024),
        Some(other) => {
            return Err(EvalError::new(
                format!("`file.read_chunk` expects a non-negative int limit, found `{other}`"),
                span,
            ));
        }
        None => {
            return Err(EvalError::new(
                "missing argument `n` for `file.read_chunk`",
                span,
            ));
        }
    };
    let path = handle.path.clone();
    run_fs(interp, span, move || {
        let Some(f) = file_lookup(id) else {
            return err_str("fs:read_chunk:closed".to_string());
        };
        let mut guard = f.file.lock().unwrap();
        let Some(fp) = guard.as_mut() else {
            return err_str("fs:read_chunk:closed".to_string());
        };
        let mut buf = vec![0u8; n];
        match fp.read(&mut buf) {
            Ok(0) => ok_str(String::new()),
            Ok(k) => {
                buf.truncate(k);
                ok_str(String::from_utf8_lossy(&buf).into_owned())
            }
            Err(e) => err_str(format!("fs:read_chunk:{}: {path}", fs_code(&e))),
        }
    })
}

pub(crate) fn file_write_chunk(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_file(args, 0, "write_chunk", "file.write_chunk")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let data = expect_str(args, 1, "file.write_chunk")?;
    let path = handle.path.clone();
    run_fs(interp, span, move || {
        let Some(f) = file_lookup(id) else {
            return err_str("fs:write_chunk:closed".to_string());
        };
        let mut guard = f.file.lock().unwrap();
        let Some(fp) = guard.as_mut() else {
            return err_str("fs:write_chunk:closed".to_string());
        };
        match fp.write_all(data.as_bytes()) {
            Ok(()) => ok_value(Value::Int(data.len() as i64)),
            Err(e) => err_str(format!("fs:write_chunk:{}: {path}", fs_code(&e))),
        }
    })
}

pub(crate) fn file_seek(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_file(args, 0, "seek", "file.seek")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let pos: u64 = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as u64,
        Some(other) => {
            return Err(EvalError::new(
                format!("`file.seek` expects a non-negative int offset, found `{other}`"),
                span,
            ));
        }
        None => {
            return Err(EvalError::new(
                "missing argument `pos` for `file.seek`",
                span,
            ));
        }
    };
    let path = handle.path.clone();
    run_fs(interp, span, move || {
        let Some(f) = file_lookup(id) else {
            return err_str("fs:seek:closed".to_string());
        };
        let mut guard = f.file.lock().unwrap();
        let Some(fp) = guard.as_mut() else {
            return err_str("fs:seek:closed".to_string());
        };
        match fp.seek(SeekFrom::Start(pos)) {
            Ok(p) => ok_value(Value::Int(p as i64)),
            Err(e) => err_str(format!("fs:seek:{}: {path}", fs_code(&e))),
        }
    })
}

pub(crate) fn file_flush(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_file(args, 0, "flush", "file.flush")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let path = handle.path.clone();
    run_fs(interp, span, move || {
        let Some(f) = file_lookup(id) else {
            return err_str("fs:flush:closed".to_string());
        };
        let mut guard = f.file.lock().unwrap();
        let Some(fp) = guard.as_mut() else {
            return err_str("fs:flush:closed".to_string());
        };
        match fp.flush() {
            Ok(()) => ok_unit(),
            Err(e) => err_str(format!("fs:flush:{}: {path}", fs_code(&e))),
        }
    })
}

pub(crate) fn file_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    // Idempotent: closing an already-closed handle is a soft `.ok` (both
    // engines agree — close never fails the program).
    let Ok((id, handle)) = expect_file(args, 0, "close", "file.close")? else {
        return Ok(ok_unit());
    };
    // Flush best-effort, take the OS handle (dropping closes the fd), and
    // remove the pool entry.
    if let Ok(mut guard) = handle.file.lock() {
        if let Some(mut fp) = guard.take() {
            let _ = fp.flush();
        }
    }
    file_drop(id);
    Ok(ok_unit())
}
