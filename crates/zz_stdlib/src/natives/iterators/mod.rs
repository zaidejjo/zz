use crate::natives::{expect_func, expect_int};
use zz_runtime::{EvalError, Interp, Value};

pub(crate) fn range(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let (start, stop, step) = match args.len() {
        1 => (0, expect_int(args, 0, "range")?, 1),
        2 => (
            expect_int(args, 0, "range")?,
            expect_int(args, 1, "range")?,
            1,
        ),
        3 => (
            expect_int(args, 0, "range")?,
            expect_int(args, 1, "range")?,
            expect_int(args, 2, "range")?,
        ),
        _ => {
            return Err(EvalError::new(
                "range expects 1, 2, or 3 arguments",
                zz_runtime::Span::new(0, 0),
            ));
        }
    };
    if step == 0 {
        return Err(EvalError::new(
            "range step cannot be zero",
            zz_runtime::Span::new(0, 0),
        ));
    }
    Ok(Value::Range(start, stop, step))
}

pub(crate) fn len(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    match args.first() {
        Some(Value::Array(vs)) => Ok(Value::Int(vs.len() as i64)),
        Some(Value::Str(s)) => Ok(Value::Int(s.chars().count() as i64)),
        Some(Value::Dict(entries)) => Ok(Value::Int(entries.len() as i64)),
        Some(Value::Range(start, stop, step)) => {
            if *step == 0 {
                Err(EvalError::new(
                    "range step cannot be zero",
                    zz_runtime::Span::new(0, 0),
                ))
            } else {
                let len = if (*step > 0 && *start < *stop) || (*step < 0 && *start > *stop) {
                    (*stop - *start + *step - if *step > 0 { 1 } else { -1 }) / *step
                } else {
                    0
                };
                Ok(Value::Int(len))
            }
        }
        Some(other) => Err(EvalError::new(
            format!("len expects array, string, dict, or range, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
        None => Err(EvalError::new(
            "missing argument for len",
            zz_runtime::Span::new(0, 0),
        )),
    }
}

/// Convert an array or range Value into a Vec<Value>.
pub(crate) fn value_to_items(v: &Value) -> Result<Vec<Value>, EvalError> {
    match v {
        Value::Array(vs) => Ok(vs.clone()),
        Value::Range(start, stop, step) => {
            let mut items = Vec::new();
            let mut i = *start;
            if *step > 0 {
                while i < *stop {
                    items.push(Value::Int(i));
                    i += *step;
                }
            } else if *step < 0 {
                while i > *stop {
                    items.push(Value::Int(i));
                    i += *step;
                }
            }
            Ok(items)
        }
        other => Err(EvalError::new(
            format!("expected array or range, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn map(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let items = value_to_items(args.first().ok_or_else(|| {
        EvalError::new(
            "missing first argument for map",
            zz_runtime::Span::new(0, 0),
        )
    })?)?;
    let f = expect_func(args, 1, "map")?;
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let call_args = vec![item];
        let res = _interp.call(f.clone(), call_args, zz_runtime::Span::new(0, 0))?;
        result.push(res);
    }
    Ok(Value::Array(result))
}

pub(crate) fn filter(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let items = value_to_items(args.first().ok_or_else(|| {
        EvalError::new(
            "missing first argument for filter",
            zz_runtime::Span::new(0, 0),
        )
    })?)?;
    let f = expect_func(args, 1, "filter")?;
    let mut result = Vec::new();
    for item in items {
        let call_args = vec![item.clone()];
        let res = _interp.call(f.clone(), call_args, zz_runtime::Span::new(0, 0))?;
        if let Value::Bool(true) = res {
            result.push(item);
        }
    }
    Ok(Value::Array(result))
}

pub(crate) fn enumerate(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let items = value_to_items(args.first().ok_or_else(|| {
        EvalError::new(
            "missing first argument for enumerate",
            zz_runtime::Span::new(0, 0),
        )
    })?)?;
    let mut result = Vec::with_capacity(items.len());
    for (i, item) in items.into_iter().enumerate() {
        result.push(Value::Tuple(vec![Value::Int(i as i64), item]));
    }
    Ok(Value::Array(result))
}

pub(crate) fn zip(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let a = value_to_items(args.first().ok_or_else(|| {
        EvalError::new(
            "missing first argument for zip",
            zz_runtime::Span::new(0, 0),
        )
    })?)?;
    let b = value_to_items(args.get(1).ok_or_else(|| {
        EvalError::new(
            "missing second argument for zip",
            zz_runtime::Span::new(0, 0),
        )
    })?)?;
    let len = a.len().min(b.len());
    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        result.push(Value::Tuple(vec![a[i].clone(), b[i].clone()]));
    }
    Ok(Value::Array(result))
}
