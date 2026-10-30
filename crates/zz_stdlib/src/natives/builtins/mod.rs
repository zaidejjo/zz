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
    // Display semantics: auto-unwrap Option (`.some(v)` → `v`,
    // `.none` → `none`); debug form stays behind `dbg(...)` / `:?`.
    Ok(Value::Str(v.to_display_string().into()))
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

/// Built-in `dbg(v)` — debug print preserving Option wrappers.
///
/// Prints the explicit debug form (`.some(v)` / `.none`) to stderr and
/// returns `v` unchanged, so it can wrap any expression inline. This is
/// the only user-facing path that keeps wrappers; `println`, string
/// interpolation, and `str()` all unwrap for display.
pub(crate) fn dbg_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for dbg", span))?;
    // `Display` is the debug form (keeps `.some`/`.none`).
    eprintln!("[dbg] {v}");
    Ok(v)
}

/// Built-in `clone(v)` — deep copy returning a fully independent value
/// (#267). Reads and binds share backing stores, so this pins a
/// snapshot regardless of ownership analysis. Cycle-safe.
pub(crate) fn clone_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for clone", span))?;
    Ok(zz_runtime::deep_clone(v))
}

/// Built-in `weak(v)` — create a weak reference to `v`, used to break
/// reference cycles (e.g. `a.b = weak(a)`). The weak reference does not
/// keep `v` alive; use `upgrade(w)` to attempt to recover it.
pub(crate) fn weak_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for weak", span))?;
    Ok(zz_runtime::weak(&v))
}

/// Built-in `upgrade(w)` — attempt to upgrade a weak reference to a
/// strong reference. Returns `None` if the referent has been freed;
/// non-weak values are returned as-is.
pub(crate) fn upgrade_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for upgrade", span))?;
    Ok(zz_runtime::upgrade(v).unwrap_or(Value::Unit))
}

// Push is `vec.push(v, x)` plus the method form `xs.push(x)`: the old
// bare `append(arr, val)` builtin was removed (single push spelling).
