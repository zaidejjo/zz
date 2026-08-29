use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn typeof_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for typeof", span))?;
    Ok(Value::Str(v.type_name()))
}

pub(crate) fn conv_str(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for str", span))?;
    Ok(Value::Str(v.to_string()))
}

pub(crate) fn conv_int(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for int", span))?;
    let result = match &v {
        Value::Int(i) => Some(*i),
        Value::Float(f) => Some(*f as i64),
        Value::Str(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    };
    Ok(Value::Option(result.map(|i| Box::new(Value::Int(i)))))
}

pub(crate) fn conv_float(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for float", span))?;
    let result = match &v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Str(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    Ok(Value::Option(result.map(|f| Box::new(Value::Float(f)))))
}
