use crate::natives::{expect_array, expect_int, expect_str};
use zz_runtime::{EvalError, Interp, Value};

pub(crate) fn vec_len(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "vec.len")?;
    Ok(Value::Int(vs.len() as i64))
}

pub(crate) fn vec_push(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.push")?;
    let x = args.get(1).cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument `x` for vec.push",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    vs.push(x);
    Ok(Value::Array(vs))
}

pub(crate) fn vec_pop(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.pop")?;
    if vs.is_empty() {
        return Err(EvalError::new(
            "vec.pop: cannot pop from an empty array",
            zz_runtime::Span::new(0, 0),
        ));
    }
    vs.pop();
    Ok(Value::Array(vs))
}

pub(crate) fn vec_reverse(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.reverse")?;
    vs.reverse();
    Ok(Value::Array(vs))
}

pub(crate) fn vec_join(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "vec.join")?;
    let sep = expect_str(args, 1, "vec.join")?;
    let parts: Vec<String> = vs.iter().map(|v| v.to_string()).collect();
    Ok(Value::Str(parts.join(&sep)))
}

pub(crate) fn vec_contains(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "vec.contains")?;
    let x = args.get(1).cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument `x` for vec.contains",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    Ok(Value::Bool(vs.contains(&x)))
}

pub(crate) fn vec_sort(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
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
    Ok(Value::Array(vs))
}

pub(crate) fn vec_insert(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.insert")?;
    let idx = expect_int(args, 1, "vec.insert")?;
    let x = args.get(2).cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument `x` for vec.insert",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    let len = vs.len() as i64;
    let idx = if idx < 0 { len + idx } else { idx };
    if idx < 0 || idx > len {
        return Err(EvalError::new(
            format!("vec.insert: index {idx} out of bounds for length {len}"),
            zz_runtime::Span::new(0, 0),
        ));
    }
    vs.insert(idx as usize, x);
    Ok(Value::Array(vs))
}

pub(crate) fn vec_remove(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "vec.remove")?;
    let idx = expect_int(args, 1, "vec.remove")?;
    let len = vs.len() as i64;
    let idx = if idx < 0 { len + idx } else { idx };
    if idx < 0 || idx >= len {
        return Err(EvalError::new(
            format!("vec.remove: index {idx} out of bounds for length {len}"),
            zz_runtime::Span::new(0, 0),
        ));
    }
    vs.remove(idx as usize);
    Ok(Value::Array(vs))
}
