use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn time_now_ms(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok(Value::Int(ms))
}

pub(crate) fn time_sleep_ms(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let ms = match args.first() {
        Some(Value::Int(ms)) => *ms,
        other => {
            return Err(EvalError::new(
                format!(
                    "sleep_ms expects `int`, found `{}`",
                    other
                        .map(|v| v.type_name())
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                span,
            ))
        }
    };
    if ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    }
    Ok(Value::Unit)
}

// --- High-resolution extension (unified with zz_native_rt::time_ext) --------

pub(crate) fn time_now_nanos(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Int(zz_native_rt::time_ext::now_nanos()))
}

pub(crate) fn time_now_micros(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Int(zz_native_rt::time_ext::now_micros()))
}

pub(crate) fn time_monotonic_nanos(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Int(zz_native_rt::time_ext::monotonic_nanos()))
}

pub(crate) fn time_sleep_micros(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let micros = match args.first() {
        Some(Value::Int(micros)) => *micros,
        other => {
            return Err(EvalError::new(
                format!(
                    "sleep_micros expects `int`, found `{}`",
                    other
                        .map(|v| v.type_name())
                        .unwrap_or_else(|| "nothing".to_string())
                ),
                span,
            ))
        }
    };
    zz_native_rt::time_ext::sleep_micros(micros);
    Ok(Value::Unit)
}
