use crate::natives::{arg, expect_str};
use zz_runtime::json::{parse_json, to_json_string, JsonValue};
use zz_runtime::{EvalError, Interp, Value};

pub(crate) fn json_parse(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.json.parse")?;
    match parse_json(&s) {
        Ok(j) => Ok(Value::Json(j)),
        Err(msg) => Err(EvalError::new(
            format!("std.json.parse: {msg}"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn json_stringify(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    let v = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for std.json.stringify",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    let j = value_to_json(&v)?;
    Ok(Value::Str(to_json_string(&j)))
}

pub(crate) fn json_get(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let j = expect_json(args, 0, "std.json.get")?;
    let key = expect_str(args, 1, "std.json.get")?;
    match j {
        JsonValue::Obj(entries) => entries
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| Value::Json(v))
            .ok_or_else(|| {
                EvalError::new(
                    format!("std.json.get: no key `{key}`"),
                    zz_runtime::Span::new(0, 0),
                )
            }),
        other => Err(EvalError::new(
            format!("std.json.get: expected an object, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn json_as_str(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_str")? {
        JsonValue::Str(s) => Ok(Value::Str(s)),
        other => Err(EvalError::new(
            format!("std.json.as_str: expected a string, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn json_as_int(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_int")? {
        JsonValue::Num(n) if n.fract() == 0.0 && n.is_finite() => Ok(Value::Int(n as i64)),
        other => Err(EvalError::new(
            format!("std.json.as_int: expected an integer, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn json_as_float(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_float")? {
        JsonValue::Num(n) => Ok(Value::Float(n)),
        other => Err(EvalError::new(
            format!("std.json.as_float: expected a number, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn json_as_bool(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_bool")? {
        JsonValue::Bool(b) => Ok(Value::Bool(b)),
        other => Err(EvalError::new(
            format!("std.json.as_bool: expected a boolean, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn expect_json(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
) -> Result<JsonValue, EvalError> {
    match arg(args, i, name)? {
        Value::Json(j) => Ok(j.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a JSON value, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

/// Convert a runtime value to a JSON value.
pub(crate) fn value_to_json(v: &Value) -> Result<JsonValue, EvalError> {
    let err = |what: &str| {
        EvalError::new(
            format!("std.json.stringify: cannot serialize {what}"),
            zz_runtime::Span::new(0, 0),
        )
    };
    match v {
        Value::Int(i) => Ok(JsonValue::Num(*i as f64)),
        Value::Float(f) => Ok(JsonValue::Num(*f)),
        Value::Str(s) => Ok(JsonValue::Str(s.clone())),
        Value::Bool(b) => Ok(JsonValue::Bool(*b)),
        Value::Unit => Ok(JsonValue::Null),
        Value::Option(Some(inner)) => value_to_json(inner),
        Value::Option(None) => Ok(JsonValue::Null),
        Value::Result(Ok(inner)) => value_to_json(inner),
        Value::Result(Err(_)) => Ok(JsonValue::Null),
        Value::Array(vs) => {
            let mut items = Vec::with_capacity(vs.len());
            for x in vs {
                items.push(value_to_json(x)?);
            }
            Ok(JsonValue::Arr(items))
        }
        Value::Dict(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for (k, val) in entries {
                let key = match k {
                    Value::Str(s) => s.clone(),
                    other => return Err(err(&format!("a non-string key (`{other}`)"))),
                };
                out.push((key, value_to_json(val)?));
            }
            Ok(JsonValue::Obj(out))
        }
        Value::Func(_) | Value::Native(_) => Err(err("a function")),
        Value::Json(j) => Ok(j.clone()),
        Value::HttpServer(_) => Err(err("an http server")),
        Value::Object { .. } => Err(err("a struct instance")),
        Value::Range(..) => Err(err("a range")),
        Value::Tuple(vs) => {
            let mut items = Vec::with_capacity(vs.len());
            for x in vs {
                items.push(value_to_json(x)?);
            }
            Ok(JsonValue::Arr(items))
        }
    }
}
