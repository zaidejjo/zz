//! `std.process` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::process` (which also backs the AOT `zz_process_*`
//! FFI). Results cross as `[status, stdout, stderr]` arrays wrapped in
//! `Result`; spawned children are [`Value::Opaque`] handles under tag
//! `"process"`.

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::{arg, expect_int, expect_str};

fn expect_argv(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
    span: Span,
) -> Result<Vec<String>, EvalError> {
    match arg(args, i, name)? {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for v in items.iter() {
                match v {
                    Value::Str(s) => out.push(s.to_string()),
                    other => {
                        return Err(EvalError::new(
                            format!("`{name}` argv must be `[str]`, found `{other}`"),
                            span,
                        ));
                    }
                }
            }
            Ok(out)
        }
        other => Err(EvalError::new(
            format!("`{name}` expects `[str]`, found `{other}`"),
            span,
        )),
    }
}

fn triple_value(status: i64, stdout: String, stderr: String) -> Value {
    Value::Array(Box::new(vec![
        Value::Int(status),
        Value::Str(stdout.into()),
        Value::Str(stderr.into()),
    ]))
}

fn ok_wrap(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

fn err_wrap(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

pub(crate) fn process_run(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let cmd = expect_str(args, 0, "std.process.run")?;
    let argv = expect_argv(args, 1, "std.process.run", _span)?;
    match zz_native_rt::process::run(&cmd, &argv, &[]) {
        Ok((st, out, err_s)) => Ok(ok_wrap(triple_value(st, out, err_s))),
        Err(e) => Ok(err_wrap(e)),
    }
}

pub(crate) fn process_run_with_env(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let cmd = expect_str(args, 0, "std.process.run_with_env")?;
    let argv = expect_argv(args, 1, "std.process.run_with_env", span)?;
    let env = expect_argv(args, 2, "std.process.run_with_env", span)?;
    let mut overlays = Vec::with_capacity(env.len());
    for e in &env {
        match e.split_once('=') {
            Some((k, v)) => overlays.push((k.to_string(), v.to_string())),
            None => {
                return Err(EvalError::new(
                    format!("`std.process.run_with_env`: env entry `{e}` is not K=V"),
                    span,
                ));
            }
        }
    }
    match zz_native_rt::process::run(&cmd, &argv, &overlays) {
        Ok((st, out, err_s)) => Ok(ok_wrap(triple_value(st, out, err_s))),
        Err(e) => Ok(err_wrap(e)),
    }
}

pub(crate) fn process_spawn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let cmd = expect_str(args, 0, "std.process.spawn")?;
    let argv = expect_argv(args, 1, "std.process.spawn", _span)?;
    match zz_native_rt::process::spawn(&cmd, &argv) {
        Ok(h) => Ok(ok_wrap(Value::Opaque(Box::new(h)))),
        Err(e) => Ok(err_wrap(e)),
    }
}

pub(crate) fn process_wait(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let id = match arg(args, 0, "std.process.wait")? {
        Value::Opaque(h) if h.tag == zz_native_rt::process::TAG => h.id,
        Value::Int(n) if *n > 0 => *n as u64,
        other => {
            return Err(EvalError::new(
                format!("`std.process.wait` expects a process handle, found `{other}`"),
                span,
            ));
        }
    };
    match zz_native_rt::process::wait(id) {
        Ok((st, out, err_s)) => Ok(ok_wrap(triple_value(st, out, err_s))),
        Err(e) => Ok(err_wrap(e.to_string())),
    }
}

pub(crate) fn process_exit(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let code = expect_int(args, 0, "std.process.exit")?;
    std::process::exit(code as i32);
}

pub(crate) fn process_pid(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Int(zz_native_rt::process::pid()))
}
