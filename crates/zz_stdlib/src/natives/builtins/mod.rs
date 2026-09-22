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
    Ok(Value::Str(v.type_name().into()))
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
    Ok(Value::Str(v.to_string().into()))
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
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        Value::Str(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        _ => f64::NAN,
    };
    Ok(Value::Float(result))
}

/// Built-in `append(arr, val)` — returns the array with val appended.
/// The compiler write-back stores the result back to `arr`, making it
/// appear to mutate in-place.
pub(crate) fn append_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "append() expects 2 arguments: append(array, value)",
            span,
        ));
    }
    let val = args.remove(1);
    match &args[0] {
        Value::Array(arr) => {
            let mut new_arr = arr.clone();
            new_arr.push(val);
            Ok(Value::Array(new_arr))
        }
        other => Err(EvalError::new(
            format!(
                "append() first argument must be an array, got `{}`",
                other.type_name()
            ),
            span,
        )),
    }
}
