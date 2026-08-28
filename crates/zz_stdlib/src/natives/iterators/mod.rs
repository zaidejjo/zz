use crate::natives::{expect_func, expect_int};
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn range(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (start, stop, step) = match args.len() {
        1 => (0, expect_int(args, 0, "range", span)?, 1),
        2 => (
            expect_int(args, 0, "range", span)?,
            expect_int(args, 1, "range", span)?,
            1,
        ),
        3 => (
            expect_int(args, 0, "range", span)?,
            expect_int(args, 1, "range", span)?,
            expect_int(args, 2, "range", span)?,
        ),
        _ => {
            return Err(EvalError::new("range expects 1, 2, or 3 arguments", span));
        }
    };
    if step == 0 {
        return Err(EvalError::new("range step cannot be zero", span));
    }
    Ok(Value::Range(start, stop, step))
}

pub(crate) fn len(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match args.first() {
        Some(Value::Array(vs)) => Ok(Value::Int(vs.len() as i64)),
        Some(Value::Str(s)) => Ok(Value::Int(s.chars().count() as i64)),
        Some(Value::Dict(entries)) => Ok(Value::Int(entries.len() as i64)),
        Some(Value::Range(start, stop, step)) => {
            if *step == 0 {
                Err(EvalError::new("range step cannot be zero", span))
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
            span,
        )),
        None => Err(EvalError::new("missing argument for len", span)),
    }
}

/// Convert an array or range Value into a Vec<Value>.
pub(crate) fn value_to_items(v: &Value, span: Span) -> Result<Vec<Value>, EvalError> {
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
            span,
        )),
    }
}

pub(crate) fn map(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let items = value_to_items(
        args.first()
            .ok_or_else(|| EvalError::new("missing first argument for map", span))?,
        span,
    )?;
    let f = expect_func(args, 1, "map", span)?;
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let call_args = vec![item];
        let res = _interp.call(f.clone(), call_args, span)?;
        result.push(res);
    }
    Ok(Value::Array(result))
}

pub(crate) fn filter(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let items = value_to_items(
        args.first()
            .ok_or_else(|| EvalError::new("missing first argument for filter", span))?,
        span,
    )?;
    let f = expect_func(args, 1, "filter", span)?;
    let mut result = Vec::new();
    for item in items {
        let call_args = vec![item.clone()];
        let res = _interp.call(f.clone(), call_args, span)?;
        if let Value::Bool(true) = res {
            result.push(item);
        }
    }
    Ok(Value::Array(result))
}

pub(crate) fn enumerate(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let items = value_to_items(
        args.first()
            .ok_or_else(|| EvalError::new("missing first argument for enumerate", span))?,
        span,
    )?;
    let mut result = Vec::with_capacity(items.len());
    for (i, item) in items.into_iter().enumerate() {
        result.push(Value::Tuple(vec![Value::Int(i as i64), item]));
    }
    Ok(Value::Array(result))
}

pub(crate) fn zip(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let a = value_to_items(
        args.first()
            .ok_or_else(|| EvalError::new("missing first argument for zip", span))?,
        span,
    )?;
    let b = value_to_items(
        args.get(1)
            .ok_or_else(|| EvalError::new("missing second argument for zip", span))?,
        span,
    )?;
    let len = a.len().min(b.len());
    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        result.push(Value::Tuple(vec![a[i].clone(), b[i].clone()]));
    }
    Ok(Value::Array(result))
}
