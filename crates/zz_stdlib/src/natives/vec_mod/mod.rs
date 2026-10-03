use crate::natives::{expect_array, expect_int, expect_str};
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn vec_len(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "vec.len")?;
    Ok(Value::Int(vs.len() as i64))
}

pub(crate) fn vec_push(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    // Move the array out of the argument slot instead of cloning it: call
    // arguments are owned per call (evaluated temps), so reusing the `Vec`
    // allocation is always sound — the caller's slot still holds its own
    // copy when the argument was read by value. This halves the per-push
    // clone cost everywhere; self-reassignment takes (`x = vec.push(x, e)`)
    // remove the remaining read clone at the statement level.
    let mut vs = take_array_arg(args, 0, "vec.push")?;
    let x = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `x` for vec.push", span))?;
    vs.push(x);
    Ok(Value::Array(Box::new(vs)))
}

/// Move the array argument out of `args[i]`, leaving `Unit` behind.
/// Restores the value and errors (like `expect_array`) on type mismatch.
fn take_array_arg(args: &mut Vec<Value>, i: usize, name: &str) -> Result<Vec<Value>, EvalError> {
    // Same errors as `expect_array` (message parity): missing slot first,
    // then the type mismatch (value restored so error paths see intact args).
    if args.get(i).is_none() {
        return Err(EvalError::new(
            format!("missing argument `{name}` for native function"),
            zz_runtime::Span::new(0, 0),
        ));
    }
    let slot = &mut args[i];
    match std::mem::replace(slot, Value::Unit) {
        Value::Array(vs) => Ok(*vs),
        other => {
            let msg = format!("`{name}` expects an array, found `{other}`");
            *slot = other;
            Err(EvalError::new(msg, zz_runtime::Span::new(0, 0)))
        }
    }
}

pub(crate) fn vec_pop(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.pop")?;
    if vs.is_empty() {
        return Err(EvalError::new(
            "vec.pop: cannot pop from an empty array",
            span,
        ));
    }
    vs.pop();
    Ok(Value::Array(Box::new(vs)))
}

pub(crate) fn vec_reverse(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.reverse")?;
    vs.reverse();
    Ok(Value::Array(Box::new(vs)))
}

pub(crate) fn vec_join(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "vec.join")?;
    let sep = expect_str(args, 1, "vec.join")?;
    // Display semantics so Options unwrap (`[.some("a")]` joins as `a`).
    let parts: Vec<String> = vs.iter().map(|v| v.to_display_string()).collect();
    Ok(Value::Str(parts.join(&sep).into()))
}

pub(crate) fn vec_contains(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "vec.contains")?;
    let x = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `x` for vec.contains", span))?;
    Ok(Value::Bool(vs.contains(&x)))
}

pub(crate) fn vec_sort(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.sort")?;
    // Sort by type name first, then by value for same types
    vs.sort_by(|a, b| {
        let ta = a.type_name();
        let tb = b.type_name();
        ta.cmp(&tb).then_with(|| match (a, b) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            }
            (Value::Str(a), Value::Str(b)) => a.cmp(b),
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            _ => std::cmp::Ordering::Equal,
        })
    });
    Ok(Value::Array(Box::new(vs)))
}

pub(crate) fn vec_insert(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.insert")?;
    let idx = expect_int(args, 1, "vec.insert")?;
    let x = args
        .get(2)
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `x` for vec.insert", span))?;
    let len = vs.len() as i64;
    let idx = if idx < 0 { len + idx } else { idx };
    if idx < 0 || idx > len {
        return Err(EvalError::new(
            format!("vec.insert: index {idx} out of bounds for length {len}"),
            span,
        ));
    }
    vs.insert(idx as usize, x);
    Ok(Value::Array(Box::new(vs)))
}

pub(crate) fn vec_remove(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.remove")?;
    let idx = expect_int(args, 1, "vec.remove")?;
    let len = vs.len() as i64;
    let idx = if idx < 0 { len + idx } else { idx };
    if idx < 0 || idx >= len {
        return Err(EvalError::new(
            format!("vec.remove: index {idx} out of bounds for length {len}"),
            span,
        ));
    }
    vs.remove(idx as usize);
    Ok(Value::Array(Box::new(vs)))
}
