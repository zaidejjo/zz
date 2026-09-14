//! `std.log` logging + tracing for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! through thin `zz_stdlib` adapters, while AOT binaries call the
//! `extern "C"` `zz_log_*` functions (same `zz_value` convention).
//!
//! Model:
//! - Levels `trace < debug < info < warn < error`, plus `off`. The global
//!   filter (default `info`) drops below-level records before formatting.
//! - Records go to **stderr** (never stdout, so `println` output and e2e
//!   markers stay clean), and optionally to an appended file sink
//!   (`to_file`). Text mode uses ANSI colors on TTYs only (piped output
//!   stays plain); JSON mode emits one `{"level","msg","ts"}` object per
//!   line with microsecond timestamps.
//! - Spans are short-lived pool handles (tag `"span"`, like `regexp`
//!   patterns): `span_begin` snapshots the monotonic clock, `span_end`
//!   returns elapsed microseconds. Forgetting `end` leaks the pool entry
//!   (documented; same policy as un-closed patterns).
//!
//! All state is process-global atomics/locks: the VM and any linked AOT
//! binary each own exactly one instance.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::cabi::{cvalue_str, cvalue_to_string, CValue};
use crate::{alloc, payload, Handle};

/// Pool tag for spans. Selects the `span.*` method namespace.
pub const SPAN_TAG: &str = "span";

/// Level codes (`set_level` parses names; `get_level` returns them).
pub const TRACE: u8 = 0;
pub const DEBUG: u8 = 1;
pub const INFO: u8 = 2;
pub const WARN: u8 = 3;
pub const ERROR: u8 = 4;
pub const OFF: u8 = 5;

static LEVEL: AtomicU8 = AtomicU8::new(INFO);
static FORMAT_JSON: AtomicU8 = AtomicU8::new(0);

/// Appended file sink (None = stderr only).
static FILE_SINK: LazyLock<Mutex<Option<std::fs::File>>> = LazyLock::new(|| Mutex::new(None));

fn level_name(level: u8) -> &'static str {
    match level {
        TRACE => "trace",
        DEBUG => "debug",
        INFO => "info",
        WARN => "warn",
        ERROR => "error",
        _ => "off",
    }
}

fn parse_level(name: &str) -> Option<u8> {
    match name {
        "trace" => Some(TRACE),
        "debug" => Some(DEBUG),
        "info" => Some(INFO),
        "warn" => Some(WARN),
        "error" => Some(ERROR),
        "off" => Some(OFF),
        _ => None,
    }
}

/// Set the global filter level. `false` (level unchanged) for unknown names.
pub fn set_level(name: &str) -> bool {
    match parse_level(name) {
        Some(l) => {
            LEVEL.store(l, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

/// Current filter level name.
pub fn get_level() -> &'static str {
    level_name(LEVEL.load(Ordering::SeqCst))
}

/// `true` when `level` passes the current filter.
pub fn enabled(level: u8) -> bool {
    level >= LEVEL.load(Ordering::SeqCst) && level <= ERROR
}

/// Select `"text"` or `"json"` record format. `false` for anything else.
pub fn set_format(name: &str) -> bool {
    match name {
        "text" => {
            FORMAT_JSON.store(0, Ordering::SeqCst);
            true
        }
        "json" => {
            FORMAT_JSON.store(1, Ordering::SeqCst);
            true
        }
        _ => false,
    }
}

/// Append records to `path` (in addition to stderr). `false` when the file
/// cannot be opened for appending.
pub fn to_file(path: &str) -> bool {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => {
            *FILE_SINK.lock().unwrap_or_else(|p| p.into_inner()) = Some(f);
            true
        }
        Err(_) => false,
    }
}

/// Drop the file sink (stderr only again).
pub fn to_stderr() {
    *FILE_SINK.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

fn colorize(level: u8, text: &str, tty: bool) -> String {
    if !tty {
        return text.to_string();
    }
    let code = match level {
        TRACE => 35,
        DEBUG => 34,
        INFO => 32,
        WARN => 33,
        ERROR => 31,
        _ => 0,
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn emit(level: u8, msg: &str) {
    if !enabled(level) {
        return;
    }
    let line = if FORMAT_JSON.load(Ordering::SeqCst) == 1 {
        let ts = crate::time_ext::now_micros();
        let obj = serde_json::json!({"level": level_name(level), "msg": msg, "ts": ts});
        serde_json::to_string(&obj)
            .unwrap_or_else(|_| format!("{{\"level\":\"{}\"}}", level_name(level)))
    } else {
        let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
        colorize(level, &format!("[{}] {msg}", level_name(level)), tty)
    };
    eprintln!("{line}");
    if let Some(f) = FILE_SINK.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        let _ = writeln!(f, "{line}");
    }
}

/// Log at `level` (checked against the filter inside [`emit`]).
pub fn log_at(level: u8, msg: &str) {
    emit(level, msg);
}

struct SpanData {
    name: String,
    start_nanos: i64,
}

/// Open a span named `name`; returns its handle.
pub fn span_begin(name: &str) -> Handle {
    alloc(
        SPAN_TAG,
        std::sync::Arc::new(SpanData {
            name: name.to_string(),
            start_nanos: crate::time_ext::monotonic_nanos(),
        }),
    )
}

/// Close the span `id`, logging `[span] <name> <elapsed>us` at info level.
/// Returns elapsed microseconds, or `None` for unknown/foreign ids.
pub fn span_end(id: u64) -> Option<i64> {
    let data: std::sync::Arc<SpanData> = payload(id, SPAN_TAG)?;
    let elapsed_us = (crate::time_ext::monotonic_nanos() - data.start_nanos) / 1_000;
    emit(INFO, &format!("[span] {} {}us", data.name, elapsed_us));
    crate::drop_handle(id);
    Some(elapsed_us)
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

/// `log.set_level(name: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_log_set_level(name: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(name) {
        Some(n) => CValue::boolean(set_level(&n)),
        None => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `log.get_level() -> str`.
#[no_mangle]
pub extern "C" fn zz_log_get_level(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    cvalue_str(get_level().as_bytes())
}

/// `log.set_format(name: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_log_set_format(name: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(name) {
        Some(n) => CValue::boolean(set_format(&n)),
        None => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `log.to_file(path: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_log_to_file(path: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(path) {
        Some(p) => CValue::boolean(to_file(&p)),
        None => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `log.to_stderr() -> unit`.
#[no_mangle]
pub extern "C" fn zz_log_to_stderr(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    to_stderr();
    CValue::unit()
}

macro_rules! level_fn {
    ($sym:ident, $level:ident) => {
        #[doc = concat!("`log.", stringify!($level), "(msg: str) -> unit`.")]
        #[no_mangle]
        pub extern "C" fn $sym(msg: CValue, err: *mut std::ffi::c_int) -> CValue {
            match cvalue_to_string(msg) {
                Some(m) => {
                    log_at($level, &m);
                    CValue::unit()
                }
                None => {
                    set_err(err);
                    CValue::unit()
                }
            }
        }
    };
}

level_fn!(zz_log_trace, TRACE);
level_fn!(zz_log_debug, DEBUG);
level_fn!(zz_log_info, INFO);
level_fn!(zz_log_warn, WARN);
level_fn!(zz_log_error, ERROR);

/// `log.span_begin(name: str) -> int` (span handle id).
#[no_mangle]
pub extern "C" fn zz_log_span_begin(name: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(name) {
        Some(n) => CValue::int(span_begin(&n).id as i64),
        None => {
            set_err(err);
            CValue::int(0)
        }
    }
}

/// `span.end(id: int) -> int` (elapsed microseconds; -1 for unknown ids).
#[no_mangle]
pub extern "C" fn zz_span_end(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    match id.as_i64() {
        Some(n) if n > 0 => match span_end(n as u64) {
            Some(us) => CValue::int(us),
            None => {
                set_err(err);
                CValue::int(-1)
            }
        },
        _ => {
            set_err(err);
            CValue::int(-1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Global log state (level, format, file sink) is process-wide, so
    /// tests that mutate it hold this lock to run serially.
    static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn level_filter_parsing() {
        let _guard = lock();
        assert!(set_level("warn"));
        assert_eq!(get_level(), "warn");
        assert!(!enabled(INFO));
        assert!(enabled(WARN));
        assert!(enabled(ERROR));
        assert!(!set_level("nope"));
        assert_eq!(get_level(), "warn", "bad name keeps level");
        assert!(set_level("trace"));
        assert!(enabled(TRACE));
        assert!(set_level("info"));
    }

    #[test]
    fn format_selection() {
        let _guard = lock();
        assert!(set_format("json"));
        assert!(!set_format("yaml"));
        assert!(set_format("text"));
    }

    #[test]
    fn file_sink_roundtrip() {
        let _guard = lock();
        assert!(set_format("text"));
        let path = std::env::temp_dir().join(format!("zz-log-test-{}", std::process::id()));
        let path_str = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        assert!(set_level("debug"));
        assert!(to_file(&path_str));
        log_at(INFO, "hello-file-sink");
        to_stderr();
        let content = std::fs::read_to_string(&path).expect("log file");
        assert!(content.contains("hello-file-sink"), "got: {content}");
        assert!(content.contains("[info]"), "got: {content}");
        let _ = std::fs::remove_file(&path);
        assert!(set_level("info"));
    }

    #[test]
    fn json_format_line() {
        let _guard = lock();
        let path = std::env::temp_dir().join(format!("zz-log-json-{}", std::process::id()));
        let path_str = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        assert!(set_format("json"));
        assert!(to_file(&path_str));
        log_at(WARN, "json-probe");
        to_stderr();
        assert!(set_format("text"));
        let content = std::fs::read_to_string(&path).expect("log file");
        let line = content.lines().next().expect("one line");
        let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
        assert_eq!(v["level"], "warn");
        assert_eq!(v["msg"], "json-probe");
        assert!(v["ts"].is_number());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn span_measures() {
        let h = span_begin("probe");
        assert_eq!(h.tag, SPAN_TAG);
        std::thread::sleep(std::time::Duration::from_millis(25));
        let us = span_end(h.id).expect("span");
        assert!(us >= 25_000, "measured {us}us");
        assert!(us < 20_000_000, "absurd: {us}us");
        assert!(span_end(h.id).is_none(), "double-end fails");
        assert!(span_end(0).is_none());
    }

    #[test]
    fn foreign_tag_rejected() {
        let h = crate::alloc("regexp", std::sync::Arc::new(1u32));
        assert!(span_end(h.id).is_none());
        assert!(crate::drop_handle(h.id));
    }
}
