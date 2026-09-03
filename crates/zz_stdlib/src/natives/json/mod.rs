use crate::natives::{arg, expect_str};
use zz_runtime::json::{parse_json, to_json_string, JsonValue};
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn json_parse(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.json.parse")?;
    match parse_json(&s) {
        Ok(j) => Ok(Value::Result(Box::new(Ok(Value::Json(Box::new(j)))))),
        Err(msg) => Ok(Value::Result(Box::new(Err(Value::Str(format!(
            "invalid JSON: {msg}"
        ).into()))))),
    }
}

pub(crate) fn json_stringify(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for std.json.stringify", _span))?;
    match value_to_json(&v) {
        Ok(j) => Ok(Value::Result(Box::new(Ok(Value::Str(to_json_string(&j).into()))))),
        Err(e) => Ok(Value::Result(Box::new(Err(Value::Str(e.message.into()))))),
    }
}

pub(crate) fn json_get(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let j = expect_json(args, 0, "std.json.get")?;
    let key = expect_str(args, 1, "std.json.get")?;
    match j {
        JsonValue::Obj(entries) => match entries.into_iter().find(|(k, _)| *k == key) {
            Some((_, v)) => Ok(Value::Result(Box::new(Ok(Value::Json(Box::new(v)))))),
            None => Ok(Value::Result(Box::new(Err(Value::Str(format!(
                "key `{key}` not found"
            ).into()))))),
        },
        JsonValue::Arr(items) => match key.parse::<usize>() {
            Ok(idx) => match items.get(idx) {
                Some(v) => Ok(Value::Result(Box::new(Ok(Value::Json(Box::new(v.clone())))))),
                None => Ok(Value::Result(Box::new(Err(Value::Str(format!(
                    "index {idx} out of bounds (len {})",
                    items.len()
                ).into()))))),
            },
            Err(_) => Ok(Value::Result(Box::new(Err(Value::Str(format!(
                "expected a numeric index for array, got `{key}`"
            ).into()))))),
        },
        other => Ok(Value::Result(Box::new(Err(Value::Str(format!(
            "expected an object or array, found `{other}`"
        ).into()))))),
    }
}

pub(crate) fn json_as_str(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_str")? {
        JsonValue::Str(s) => Ok(Value::Str(s.into())),
        other => Err(EvalError::new(
            format!("std.json.as_str: expected a string, found `{other}`"),
            span,
        )),
    }
}

pub(crate) fn json_as_int(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_int")? {
        JsonValue::Num(n) if n.fract() == 0.0 && n.is_finite() => Ok(Value::Int(n as i64)),
        other => Err(EvalError::new(
            format!("std.json.as_int: expected an integer, found `{other}`"),
            span,
        )),
    }
}

pub(crate) fn json_as_float(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_float")? {
        JsonValue::Num(n) => Ok(Value::Float(n)),
        other => Err(EvalError::new(
            format!("std.json.as_float: expected a number, found `{other}`"),
            span,
        )),
    }
}

pub(crate) fn json_as_bool(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match expect_json(args, 0, "std.json.as_bool")? {
        JsonValue::Bool(b) => Ok(Value::Bool(b)),
        other => Err(EvalError::new(
            format!("std.json.as_bool: expected a boolean, found `{other}`"),
            span,
        )),
    }
}

pub(crate) fn json_null(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Json(Box::new(JsonValue::Null)))
}

pub(crate) fn expect_json(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
) -> Result<JsonValue, EvalError> {
    match arg(args, i, name)? {
        Value::Json(j) => Ok((**j).clone()),
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
        Value::Str(s) => Ok(JsonValue::Str((**s).clone())),
        Value::Bool(b) => Ok(JsonValue::Bool(*b)),
        Value::Unit => Ok(JsonValue::Null),
        Value::Option(Some(inner)) => value_to_json(inner),
        Value::Option(None) => Ok(JsonValue::Null),
        Value::Result(r) => match &**r {
                            Ok(inner) => value_to_json(inner),
                            Err(_) => Ok(JsonValue::Null),
                        },
        
        Value::Array(vs) => {
            let mut items = Vec::with_capacity(vs.len());
            for x in &**vs {
                items.push(value_to_json(x)?);
            }
            Ok(JsonValue::Arr(items))
        }
        Value::Dict(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for (k, val) in &**entries {
                let key = match k {
                    Value::Str(s) => s.clone(),
                    other => return Err(err(&format!("a non-string key (`{other}`)"))),
                };
                out.push(((*key).clone(), value_to_json(val)?));
            }
            Ok(JsonValue::Obj(out))
        }
        Value::Func(_) | Value::Native(_) => Err(err("a function")),
        Value::Json(j) => Ok((**j).clone()),
        Value::HttpServer(_) => Err(err("an http server")),
        Value::TcpStream(_) => Err(err("a tcp stream")),
        Value::TcpListener(_) => Err(err("a tcp listener")),
        Value::Response(_) => Err(err("an http response")),
        Value::Object { .. } => Err(err("a struct instance")),
        Value::Range(..) => Err(err("a range")),
        Value::Tuple(vs) => {
            let mut items = Vec::with_capacity(vs.len());
            for x in &**vs {
                items.push(value_to_json(x)?);
            }
            Ok(JsonValue::Arr(items))
        }
    }
}
