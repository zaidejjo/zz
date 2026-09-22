//! `std.env` — cross-platform environment, directories, and OS identity.
//!
//! Thin wrappers over [`std::env`]: every fallible syscall surfaces as
//! `Result<_, str>` so VM and AOT (`zz_env_*` in `core.c`) agree
//! byte-for-byte. Pure queries (`temp_dir`, `os`) are total.

use crate::natives::expect_str;
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn env_get_var(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.env.get_var")?;
    match std::env::var(&name) {
        Ok(v) => Ok(Value::Option(Some(Box::new(Value::Str(v.into()))))),
        Err(_) => Ok(Value::Option(None)),
    }
}

pub(crate) fn env_var(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.env.var")?;
    match std::env::var(&name) {
        Ok(v) => Ok(Value::Result(Box::new(Ok(Value::Str(v.into()))))),
        Err(_) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("environment variable `{name}` not set").into(),
        ))))),
    }
}

pub(crate) fn env_args(
    interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Array(Box::new(
        interp
            .args
            .iter()
            .map(|s| Value::Str(s.clone().into()))
            .collect(),
    )))
}

/// `env.get(key) -> Option<str>`: value or `.none` (never fails — a
/// missing variable or a non-Unicode value both yield `.none`, mirroring
/// `get_var`).
pub(crate) fn env_get(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.env.get")?;
    match std::env::var(&name) {
        Ok(v) => Ok(Value::Option(Some(Box::new(Value::Str(v.into()))))),
        Err(_) => Ok(Value::Option(None)),
    }
}

fn invalid_key(key: &str) -> bool {
    key.is_empty() || key.bytes().any(|b| b == 0 || b == b'=')
}

/// `env.set(key, val) -> Result<unit, str>`: only empty keys, `=`, and
/// NUL bytes can fail (the OS rejects those).
pub(crate) fn env_set(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.env.set")?;
    let val = expect_str(args, 1, "std.env.set")?;
    if invalid_key(&name) {
        return Ok(Value::Result(Box::new(Err(Value::Str(
            format!("invalid environment variable name `{name}`").into(),
        )))));
    }
    // SAFETY: `set_var` is process-global and not `Sync`-safe under
    // concurrent `getenv`; ZZ programs are single-process scripts and
    // the stdlib documents set/remove as process-wide (same contract as
    // the AOT `zz_env_set`, which calls `_putenv`/`setenv` directly).
    unsafe { std::env::set_var(&name, &val) };
    Ok(Value::Result(Box::new(Ok(Value::Unit))))
}

/// `env.remove(key)` / `env.unset(key)`: total no-op when absent.
pub(crate) fn env_remove(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let name = expect_str(args, 0, "std.env.remove")?;
    // SAFETY: see `env_set` — process-wide by contract.
    unsafe { std::env::remove_var(&name) };
    Ok(Value::Unit)
}

/// `env.vars() -> Dict<str, str>`: snapshot of the whole environment.
/// Non-Unicode entries are skipped (keys/values must be ZZ strings).
pub(crate) fn env_vars(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let mut pairs: Vec<(Value, Value)> = std::env::vars()
        .map(|(k, v)| (Value::Str(k.into()), Value::Str(v.into())))
        .collect();
    pairs.sort_by(|a, b| {
        let ka = match &a.0 {
            Value::Str(s) => s.to_string(),
            other => other.to_string(),
        };
        let kb = match &b.0 {
            Value::Str(s) => s.to_string(),
            other => other.to_string(),
        };
        ka.cmp(&kb)
    });
    Ok(Value::Dict(Box::new(pairs)))
}

/// `env.cwd() -> Result<str, str>`.
pub(crate) fn env_cwd(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match std::env::current_dir() {
        Ok(p) => Ok(Value::Result(Box::new(Ok(Value::Str(
            p.to_string_lossy().into_owned().into(),
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("cannot read working directory: {e}").into(),
        ))))),
    }
}

/// `env.set_cwd(path) -> Result<unit, str>`.
pub(crate) fn env_set_cwd(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.env.set_cwd")?;
    match std::env::set_current_dir(&path) {
        Ok(()) => Ok(Value::Result(Box::new(Ok(Value::Unit)))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("fs:set_cwd:io_error: {path} ({e})").into(),
        ))))),
    }
}

/// `env.exe_path() -> Result<str, str>`: absolute path of this binary.
pub(crate) fn env_exe_path(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match std::env::current_exe() {
        Ok(p) => Ok(Value::Result(Box::new(Ok(Value::Str(
            p.to_string_lossy().into_owned().into(),
        ))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(
            format!("cannot read executable path: {e}").into(),
        ))))),
    }
}

/// `env.home_dir() -> Option<str>` (`HOME` on Unix, `USERPROFILE` on
/// Windows — same rule the CLI itself uses for `~/.zz`).
pub(crate) fn env_home_dir(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Ok(Value::Option(Some(Box::new(Value::Str(v.into()))))),
        _ => Ok(Value::Option(None)),
    }
}

/// `env.temp_dir() -> str`: total (`std::env::temp_dir` never fails —
/// `$TMPDIR` / `/tmp` on Unix, `TMP`/`TEMP`/Windows dir on Windows).
pub(crate) fn env_temp_dir(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Str(
        std::env::temp_dir().to_string_lossy().into_owned().into(),
    ))
}

/// `env.user() -> Option<str>` (`USER`/`LOGNAME` on Unix, `USERNAME` on
/// Windows).
pub(crate) fn env_user(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    #[cfg(windows)]
    let keys: &[&str] = &["USERNAME"];
    #[cfg(not(windows))]
    let keys: &[&str] = &["USER", "LOGNAME"];
    for k in keys {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return Ok(Value::Option(Some(Box::new(Value::Str(v.into())))));
            }
        }
    }
    Ok(Value::Option(None))
}

/// `env.os() -> str`: `"linux"`, `"macos"`, `"windows"`, … (compile-time
/// constant, identical in both engines).
pub(crate) fn env_os(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Str(zz_native_rt::sys::os().to_string().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val_to_string(v: &Value) -> String {
        v.to_string()
    }

    #[test]
    fn get_set_remove_roundtrip() {
        let mut interp = Interp::new();
        let key = "ZZ_STD_ENV_TEST_ROUNDTRIP";
        let mut rm = vec![Value::Str(key.to_string().into())];
        env_remove(&mut interp, &mut rm, Span::new(0, 0)).unwrap();
        let mut get = vec![Value::Str(key.to_string().into())];
        assert_eq!(
            val_to_string(&env_get(&mut interp, &mut get, Span::new(0, 0)).unwrap()),
            ".none"
        );
        let mut set = vec![
            Value::Str(key.to_string().into()),
            Value::Str("hello".to_string().into()),
        ];
        assert_eq!(
            val_to_string(&env_set(&mut interp, &mut set, Span::new(0, 0)).unwrap()),
            ".ok()"
        );
        let mut get = vec![Value::Str(key.to_string().into())];
        assert_eq!(
            val_to_string(&env_get(&mut interp, &mut get, Span::new(0, 0)).unwrap()),
            ".some(hello)"
        );
        let mut rm = vec![Value::Str(key.to_string().into())];
        env_remove(&mut interp, &mut rm, Span::new(0, 0)).unwrap();
        let mut get = vec![Value::Str(key.to_string().into())];
        assert_eq!(
            val_to_string(&env_get(&mut interp, &mut get, Span::new(0, 0)).unwrap()),
            ".none"
        );
    }

    #[test]
    fn set_rejects_bad_keys() {
        let mut interp = Interp::new();
        for bad in ["", "A=B", "A\0B"] {
            let mut args = vec![
                Value::Str(bad.to_string().into()),
                Value::Str("x".to_string().into()),
            ];
            let v = env_set(&mut interp, &mut args, Span::new(0, 0)).unwrap();
            assert!(
                val_to_string(&v).starts_with(".err("),
                "expected .err for {bad:?}, got {v}"
            );
        }
    }

    #[test]
    fn vars_cwd_exe_temp_os_shapes() {
        let mut interp = Interp::new();
        let mut no_args: Vec<Value> = vec![];
        let vars = env_vars(&mut interp, &mut no_args, Span::new(0, 0)).unwrap();
        assert!(matches!(vars, Value::Dict(_)));
        let cwd = env_cwd(&mut interp, &mut no_args, Span::new(0, 0)).unwrap();
        assert!(val_to_string(&cwd).starts_with(".ok("));
        let exe = env_exe_path(&mut interp, &mut no_args, Span::new(0, 0)).unwrap();
        assert!(val_to_string(&exe).starts_with(".ok("));
        let tmp = env_temp_dir(&mut interp, &mut no_args, Span::new(0, 0)).unwrap();
        assert!(!val_to_string(&tmp).is_empty());
        let os = env_os(&mut interp, &mut no_args, Span::new(0, 0)).unwrap();
        assert!(["linux", "macos", "windows"]
            .iter()
            .any(|o| val_to_string(&os) == *o));
    }

    #[test]
    fn set_cwd_roundtrip() {
        let mut interp = Interp::new();
        let start = std::env::current_dir().unwrap();
        let tmp = std::env::temp_dir();
        let mut args = vec![Value::Str(tmp.to_string_lossy().into_owned().into())];
        assert_eq!(
            val_to_string(&env_set_cwd(&mut interp, &mut args, Span::new(0, 0)).unwrap()),
            ".ok()"
        );
        assert_eq!(std::env::current_dir().unwrap(), tmp);
        let mut back = vec![Value::Str(start.to_string_lossy().into_owned().into())];
        env_set_cwd(&mut interp, &mut back, Span::new(0, 0)).unwrap();
        assert_eq!(std::env::current_dir().unwrap(), start);
        // Missing dir is a loud .err, never a panic.
        let mut bad = vec![Value::Str(
            "/zz-definitely-missing-dir-xyz".to_string().into(),
        )];
        let v = env_set_cwd(&mut interp, &mut bad, Span::new(0, 0)).unwrap();
        assert!(val_to_string(&v).starts_with(".err("));
    }
}
