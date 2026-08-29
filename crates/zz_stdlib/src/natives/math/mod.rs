use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn math_abs(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for abs", span))?;
    match v {
        Value::Int(i) => Ok(Value::Int(i.abs())),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        other => Err(EvalError::new(
            format!(
                "abs expects `int` or `float`, found `{}`",
                other.type_name()
            ),
            span,
        )),
    }
}

pub(crate) fn math_floor(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for floor", span))?;
    match v {
        Value::Float(f) => Ok(Value::Int(f.floor() as i64)),
        Value::Int(i) => Ok(Value::Int(i)),
        other => Err(EvalError::new(
            format!("floor expects `float`, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_ceil(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for ceil", span))?;
    match v {
        Value::Float(f) => Ok(Value::Int(f.ceil() as i64)),
        Value::Int(i) => Ok(Value::Int(i)),
        other => Err(EvalError::new(
            format!("ceil expects `float`, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_sqrt(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for sqrt", span))?;
    match v {
        Value::Int(i) => Ok(Value::Float((i as f64).sqrt())),
        Value::Float(f) => Ok(Value::Float(f.sqrt())),
        other => Err(EvalError::new(
            format!(
                "sqrt expects `int` or `float`, found `{}`",
                other.type_name()
            ),
            span,
        )),
    }
}

pub(crate) fn math_pow(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let base = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for pow", span))?;
    let exp = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for pow", span))?;
    let to_f = |v: &Value| -> Option<f64> {
        match v {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    };
    let (b, e) = match (to_f(&base), to_f(&exp)) {
        (Some(b), Some(e)) => (b, e),
        _ => {
            return Err(EvalError::new(
                format!(
                    "pow expects `int` or `float` arguments, found `{}` and `{}`",
                    base.type_name(),
                    exp.type_name()
                ),
                span,
            ))
        }
    };
    Ok(Value::Float(b.powf(e)))
}

pub(crate) fn math_random(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    // Simple LCG seeded from the clock — no external RNG dependency.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut state = (nanos as u64) | 1;
    state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let unit = (state >> 33) as f64 / (1u64 << 31) as f64;
    Ok(Value::Float(unit))
}
