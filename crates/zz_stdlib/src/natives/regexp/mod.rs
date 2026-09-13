//! `std.regexp` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::regexp` (which also backs the AOT `zz_regexp_*` FFI).
//! Compiled patterns are [`Value::Opaque`] handles under tag `"regexp"`,
//! so `re.is_match(s)` method syntax works through the standard tag-based
//! dispatch.

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::{arg, expect_str};

fn expect_regexp(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
    span: Span,
) -> Result<zz_native_rt::Handle, EvalError> {
    match arg(args, i, name)? {
        Value::Opaque(h) if h.tag == zz_native_rt::regexp::TAG => Ok((**h).clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a regexp handle, found `{other}`"),
            span,
        )),
    }
}

fn gone(name: &str, span: Span) -> EvalError {
    EvalError::new(format!("`{name}`: regexp handle is no longer valid"), span)
}

fn ok_wrap(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

fn err_wrap(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

pub(crate) fn regexp_compile(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let pat = expect_str(args, 0, "std.regexp.compile")?;
    match zz_native_rt::regexp::compile(&pat) {
        Ok(h) => Ok(ok_wrap(Value::Opaque(Box::new(h)))),
        Err(msg) => Ok(err_wrap(msg)),
    }
}

pub(crate) fn regexp_is_match(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_regexp(args, 0, "std.regexp.is_match", span)?;
    let s = expect_str(args, 1, "std.regexp.is_match")?;
    match zz_native_rt::regexp::with(h.id, |rx| rx.is_match(&s)) {
        Some(m) => Ok(Value::Bool(m)),
        None => Err(gone("std.regexp.is_match", span)),
    }
}

pub(crate) fn regexp_find(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_regexp(args, 0, "std.regexp.find", span)?;
    let s = expect_str(args, 1, "std.regexp.find")?;
    match zz_native_rt::regexp::with(h.id, |rx| rx.find(&s).map(|m| m.as_str().to_string())) {
        Some(Some(m)) => Ok(Value::Option(Some(Box::new(Value::Str(m.into()))))),
        Some(None) => Ok(Value::Option(None)),
        None => Err(gone("std.regexp.find", span)),
    }
}

pub(crate) fn regexp_replace_all(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_regexp(args, 0, "std.regexp.replace_all", span)?;
    let s = expect_str(args, 1, "std.regexp.replace_all")?;
    let rep = expect_str(args, 2, "std.regexp.replace_all")?;
    match zz_native_rt::regexp::with(h.id, |rx| rx.replace_all(&s, rep.as_str()).into_owned()) {
        Some(out) => Ok(Value::Str(out.into())),
        None => Err(gone("std.regexp.replace_all", span)),
    }
}

pub(crate) fn regexp_captures(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_regexp(args, 0, "std.regexp.captures", span)?;
    let s = expect_str(args, 1, "std.regexp.captures")?;
    match zz_native_rt::regexp::with(h.id, |rx| {
        rx.captures(&s).map(|caps| {
            caps.iter()
                .map(|g| Value::Str(g.map(|m| m.as_str().to_string()).unwrap_or_default().into()))
                .collect::<Vec<_>>()
        })
    }) {
        Some(Some(groups)) => Ok(Value::Array(Box::new(groups))),
        Some(None) => Ok(Value::Array(Box::default())),
        None => Err(gone("std.regexp.captures", span)),
    }
}
