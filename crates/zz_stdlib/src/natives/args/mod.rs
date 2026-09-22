//! `std.args` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::args` (which also backs the AOT `zz_args_*` FFI).
//! Parsers are [`Value::Opaque`] handles under tag `"args"`, so
//! `p.parse(argv)` method syntax works through the standard tag-based
//! dispatch. Raw argv comes from `interp.args` (same source the AOT
//! `zz_env_args` reads from the C globals).

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::{arg, expect_int, expect_str};

pub(crate) fn args_get_raw(
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

fn expect_parser(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
    span: Span,
) -> Result<zz_native_rt::Handle, EvalError> {
    match arg(args, i, name)? {
        Value::Opaque(h) if h.tag == zz_native_rt::args::TAG => Ok((**h).clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an args parser handle, found `{other}`"),
            span,
        )),
    }
}

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

pub(crate) fn args_parser(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Opaque(Box::new(zz_native_rt::args::parser_new())))
}

pub(crate) fn args_str_flag(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.str_flag", span)?;
    let name = expect_str(args, 1, "std.args.str_flag")?;
    let default = expect_str(args, 2, "std.args.str_flag")?;
    if !zz_native_rt::args::str_flag(h.id, &name, &default) {
        return Err(EvalError::new(
            "`std.args.str_flag`: parser handle is no longer valid".to_string(),
            span,
        ));
    }
    Ok(Value::Unit)
}

pub(crate) fn args_int_flag(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.int_flag", span)?;
    let name = expect_str(args, 1, "std.args.int_flag")?;
    let default = expect_int(args, 2, "std.args.int_flag")?;
    if !zz_native_rt::args::int_flag(h.id, &name, default) {
        return Err(EvalError::new(
            "`std.args.int_flag`: parser handle is no longer valid".to_string(),
            span,
        ));
    }
    Ok(Value::Unit)
}

pub(crate) fn args_bool_flag(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.bool_flag", span)?;
    let name = expect_str(args, 1, "std.args.bool_flag")?;
    if !zz_native_rt::args::bool_flag(h.id, &name) {
        return Err(EvalError::new(
            "`std.args.bool_flag`: parser handle is no longer valid".to_string(),
            span,
        ));
    }
    Ok(Value::Unit)
}

pub(crate) fn args_parse(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.parse", span)?;
    let argv = expect_argv(args, 1, "std.args.parse", span)?;
    match zz_native_rt::args::parse(h.id, &argv) {
        Some(ok) => Ok(Value::Bool(ok)),
        None => Err(EvalError::new(
            "`std.args.parse`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}

pub(crate) fn args_get_str(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.get_str", span)?;
    let name = expect_str(args, 1, "std.args.get_str")?;
    match zz_native_rt::args::get_str(h.id, &name) {
        Some(s) => Ok(Value::Str(s.into())),
        None => Err(EvalError::new(
            "`std.args.get_str`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}

pub(crate) fn args_get_int(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.get_int", span)?;
    let name = expect_str(args, 1, "std.args.get_int")?;
    match zz_native_rt::args::get_int(h.id, &name) {
        Some(n) => Ok(Value::Int(n)),
        None => Err(EvalError::new(
            "`std.args.get_int`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}

pub(crate) fn args_get_bool(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.get_bool", span)?;
    let name = expect_str(args, 1, "std.args.get_bool")?;
    match zz_native_rt::args::get_bool(h.id, &name) {
        Some(b) => Ok(Value::Bool(b)),
        None => Err(EvalError::new(
            "`std.args.get_bool`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}

pub(crate) fn args_positional(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.positional", span)?;
    let i = expect_int(args, 1, "std.args.positional")?;
    if i < 0 {
        return Err(EvalError::new(
            "`std.args.positional`: index must be >= 0".to_string(),
            span,
        ));
    }
    match zz_native_rt::args::positional(h.id, i as usize) {
        Some(Some(s)) => Ok(Value::Option(Some(Box::new(Value::Str(s.into()))))),
        Some(None) => Ok(Value::Option(None)),
        None => Err(EvalError::new(
            "`std.args.positional`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}

pub(crate) fn args_subcommand(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.subcommand", span)?;
    match zz_native_rt::args::subcommand(h.id) {
        Some(s) => Ok(Value::Str(s.into())),
        None => Err(EvalError::new(
            "`std.args.subcommand`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}

pub(crate) fn args_help(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.help", span)?;
    let prog = expect_str(args, 1, "std.args.help")?;
    match zz_native_rt::args::help_of(h.id, &prog) {
        Some(s) => Ok(Value::Str(s.into())),
        None => Err(EvalError::new(
            "`std.args.help`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}

pub(crate) fn args_error(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.error", span)?;
    match zz_native_rt::args::error_of(h.id) {
        Some(s) => Ok(Value::Str(s.into())),
        None => Err(EvalError::new(
            "`std.args.error`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}

pub(crate) fn args_was_help(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let h = expect_parser(args, 0, "std.args.was_help", span)?;
    match zz_native_rt::args::was_help(h.id) {
        Some(b) => Ok(Value::Bool(b)),
        None => Err(EvalError::new(
            "`std.args.was_help`: parser handle is no longer valid".to_string(),
            span,
        )),
    }
}
