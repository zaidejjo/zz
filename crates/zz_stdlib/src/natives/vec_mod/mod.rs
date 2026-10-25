use crate::natives::{expect_array, expect_func, expect_int, expect_str};
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
    Ok(Value::Array(std::sync::Arc::new(vs)))
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
        // Owned take: reuse the buffer when uniquely owned, copy once
        // otherwise (the copy value semantics always required).
        Value::Array(vs) => Ok(std::sync::Arc::try_unwrap(vs).unwrap_or_else(|a| (*a).clone())),
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
    let mut vs = take_array_arg(args, 0, "vec.pop")?;
    if vs.is_empty() {
        return Err(EvalError::new(
            "vec.pop: cannot pop from an empty array",
            span,
        ));
    }
    vs.pop();
    Ok(Value::Array(std::sync::Arc::new(vs)))
}

pub(crate) fn vec_reverse(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let mut vs = take_array_arg(args, 0, "vec.reverse")?;
    vs.reverse();
    Ok(Value::Array(std::sync::Arc::new(vs)))
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

/// A homogeneous scalar array lane: `vec.sort` can order it without asking
/// either side for its `type_name()`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScalarLane {
    Int,
    Float,
    Bool,
    Str,
}

fn scalar_lane(v: &Value) -> Option<ScalarLane> {
    match v {
        Value::Int(_) => Some(ScalarLane::Int),
        Value::Float(_) => Some(ScalarLane::Float),
        Value::Bool(_) => Some(ScalarLane::Bool),
        Value::Str(_) => Some(ScalarLane::Str),
        _ => None,
    }
}

pub(crate) fn vec_sort(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let mut vs = take_array_arg(args, 0, "vec.sort")?;
    // The generic comparator below asks both sides for `type_name()`, which
    // builds a fresh `String` — twice per comparison, so a 200 k-element
    // `[int]` sort did ~6.4 M allocations and spent its time in `malloc`.
    // A homogeneous scalar array needs none of that: the type-name key is
    // constant and the value comparison is the one the generic arm already
    // used. Mirrors the AOT runtime's scalar lanes in `zz_vec_sort`.
    //
    // Stability: the int lane is unstable, which is unobservable because
    // equal ints are indistinguishable (same argument as the C twin); the
    // other lanes keep `sort_by` (stable). Mixed arrays fall through.
    if let Some(lane) = vs.first().and_then(scalar_lane) {
        if vs.iter().all(|v| scalar_lane(v) == Some(lane)) {
            match lane {
                ScalarLane::Int => {
                    let mut keys: Vec<i64> = Vec::with_capacity(vs.len());
                    keys.extend(vs.iter().filter_map(|v| match v {
                        Value::Int(i) => Some(*i),
                        _ => None,
                    }));
                    keys.sort_unstable();
                    for (slot, key) in vs.iter_mut().zip(keys) {
                        *slot = Value::Int(key);
                    }
                }
                ScalarLane::Float => vs.sort_by(|a, b| match (a, b) {
                    (Value::Float(a), Value::Float(b)) => {
                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    _ => std::cmp::Ordering::Equal,
                }),
                ScalarLane::Bool => vs.sort_by(|a, b| match (a, b) {
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    _ => std::cmp::Ordering::Equal,
                }),
                ScalarLane::Str => vs.sort_by(|a, b| match (a, b) {
                    (Value::Str(a), Value::Str(b)) => a.cmp(b),
                    _ => std::cmp::Ordering::Equal,
                }),
            }
            return Ok(Value::Array(std::sync::Arc::new(vs)));
        }
    }
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
    Ok(Value::Array(std::sync::Arc::new(vs)))
}

/// Compare two values by natural order (int < float < str < bool by tag,
/// then within-tag comparison). Used by `sort_key`.
fn cmp_value(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        // Different tags: order by tag name for determinism.
        _ => a.type_name().cmp(&b.type_name()),
    }
}

/// Interpret a comparator's return value as an ordering: negative -> Less,
/// zero -> Equal, positive -> Greater. Accepts int, float, bool.
fn cmp_result(v: &Value) -> std::cmp::Ordering {
    match v {
        Value::Int(n) => n.cmp(&0),
        Value::Float(n) => n.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal),
        Value::Bool(true) => std::cmp::Ordering::Greater,
        Value::Bool(false) => std::cmp::Ordering::Less,
        _ => std::cmp::Ordering::Equal,
    }
}

pub(crate) fn vec_sort_by(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vs = take_array_arg(args, 0, "vec.sort_by")?;
    let cmp = expect_func(args, 1, "vec.sort_by")?;
    let err = std::cell::Cell::new(None as Option<EvalError>);
    vs.sort_by(
        |a, b| match interp.call(cmp.clone(), vec![a.clone(), b.clone()], span) {
            Ok(v) => cmp_result(&v),
            Err(e) => {
                err.set(Some(e));
                std::cmp::Ordering::Equal
            }
        },
    );
    if let Some(e) = err.take() {
        return Err(e);
    }
    Ok(Value::Array(std::sync::Arc::new(vs)))
}

pub(crate) fn vec_sort_by_desc(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vs = take_array_arg(args, 0, "vec.sort_by_desc")?;
    let cmp = expect_func(args, 1, "vec.sort_by_desc")?;
    let err = std::cell::Cell::new(None as Option<EvalError>);
    vs.sort_by(
        |a, b| match interp.call(cmp.clone(), vec![a.clone(), b.clone()], span) {
            Ok(v) => cmp_result(&v).reverse(),
            Err(e) => {
                err.set(Some(e));
                std::cmp::Ordering::Equal
            }
        },
    );
    if let Some(e) = err.take() {
        return Err(e);
    }
    Ok(Value::Array(std::sync::Arc::new(vs)))
}

pub(crate) fn vec_sort_key(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let vs = take_array_arg(args, 0, "vec.sort_key")?;
    let key = expect_func(args, 1, "vec.sort_key")?;
    let err = std::cell::Cell::new(None as Option<EvalError>);
    // Decorate: compute key for each element once (stable, O(n) key calls).
    let mut decorated: Vec<(Value, Value)> = Vec::with_capacity(vs.len());
    for item in vs.iter() {
        match interp.call(key.clone(), vec![item.clone()], span) {
            Ok(k) => decorated.push((item.clone(), k)),
            Err(e) => {
                err.set(Some(e));
                break;
            }
        }
    }
    if let Some(e) = err.take() {
        return Err(e);
    }
    // Sort by key (stable), then undecorate.
    decorated.sort_by(|a, b| cmp_value(&a.1, &b.1));
    let sorted: Vec<Value> = decorated.into_iter().map(|(item, _)| item).collect();
    Ok(Value::Array(std::sync::Arc::new(sorted)))
}

pub(crate) fn vec_insert(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vs = take_array_arg(args, 0, "vec.insert")?;
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
    Ok(Value::Array(std::sync::Arc::new(vs)))
}

pub(crate) fn vec_remove(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vs = take_array_arg(args, 0, "vec.remove")?;
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
    Ok(Value::Array(std::sync::Arc::new(vs)))
}
