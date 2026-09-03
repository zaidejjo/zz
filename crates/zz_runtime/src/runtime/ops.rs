//! Shared operations on runtime values, used by both the tree-walker
//! interpreter and the bytecode VM.
//!
//! All functions here are `pub(crate)` standalone functions — they do NOT
//! take `&mut self` and can be called from any module in the crate.

use zz_frontend::ast::{BinOp, UnOp};
use zz_frontend::span::Span;

use super::EvalError;
use crate::value::Value;

// ---------------------------------------------------------------------------
// Object / field helpers
// ---------------------------------------------------------------------------

/// Read a field from a struct instance.
#[inline(always)]
pub(crate) fn object_field(obj: &Value, name: &str, span: Span) -> Result<Value, EvalError> {
    match obj {
        Value::Object(o) => o
            .fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| EvalError::new(format!("struct `{}` has no field `{name}`", o.name), span)),
        Value::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, Value::Str(s) if &**s == name))
            .map(|(_, v)| v.clone())
            .ok_or_else(|| EvalError::new(format!("dict has no key `{name}`"), span)),
        other => Err(EvalError::new(
            format!("cannot access field `{name}` on a value of type `{other}`"),
            span,
        )),
    }
}

/// Write a field into a struct instance (in place).
pub(crate) fn set_object_field(
    obj: &mut Value,
    name: &str,
    value: Value,
    span: Span,
) -> Result<(), EvalError> {
    match obj {
        Value::Object(o) => {
            if let Some((_, slot)) = o.fields.iter_mut().find(|(n, _)| n == name) {
                *slot = value;
                Ok(())
            } else {
                Err(EvalError::new(
                    format!("struct `{}` has no field `{name}`", o.name),
                    span,
                ))
            }
        }
        other => Err(EvalError::new(
            format!("cannot assign to field `{name}` of a value of type `{other}`"),
            span,
        )),
    }
}

// ---------------------------------------------------------------------------
// Indexing helpers
// ---------------------------------------------------------------------------

/// Normalize an index (negative counts from the end) and bounds-check it.
#[inline(always)]
pub(crate) fn normalize_index(i: i64, len: usize, span: Span) -> Result<usize, EvalError> {
    let len_i = len as i64;
    let idx = if i < 0 { len_i + i } else { i };
    if idx < 0 || idx >= len_i {
        return Err(EvalError::new(
            format!("index {i} out of bounds for length {len}"),
            span,
        ));
    }
    Ok(idx as usize)
}

/// Read an element: `arr[i]`, `dict[key]`, `str[i]`. Negative indices
/// count from the end.
#[inline(always)]
pub(crate) fn get_index(obj: &Value, index: &Value, span: Span) -> Result<Value, EvalError> {
    match (obj, index) {
        (Value::Array(items), Value::Int(i)) => {
            let idx = normalize_index(*i, items.len(), span)?;
            Ok(items[idx].clone())
        }
        (Value::Dict(entries), key) => entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| EvalError::new(format!("key `{key}` not found in dict"), span)),
        (Value::Str(s), Value::Int(i)) => {
            let chars: Vec<char> = s.chars().collect();
            let idx = normalize_index(*i, chars.len(), span)?;
            Ok(Value::Str(chars[idx].to_string().into()))
        }
        (other, _) => Err(EvalError::new(
            format!("cannot index a value of type `{}`", other.type_name()),
            span,
        )),
    }
}

/// Write an element: `arr[i] = v`, `dict[key] = v`. Missing dict keys
/// are appended; strings are immutable.
pub(crate) fn set_index(
    obj: &mut Value,
    index: &Value,
    value: Value,
    span: Span,
) -> Result<(), EvalError> {
    match (obj, index) {
        (Value::Array(items), Value::Int(i)) => {
            let idx = normalize_index(*i, items.len(), span)?;
            items[idx] = value;
            Ok(())
        }
        (Value::Dict(entries), key) => {
            if let Some((_, slot)) = entries.iter_mut().find(|(k, _)| k == key) {
                *slot = value;
            } else {
                entries.push((key.clone(), value));
            }
            Ok(())
        }
        (Value::Str(_), _) => Err(EvalError::new(
            "cannot assign to an index of a string",
            span,
        )),
        (other, _) => Err(EvalError::new(
            format!(
                "cannot assign to an index of a value of type `{}`",
                other.type_name()
            ),
            span,
        )),
    }
}

// ---------------------------------------------------------------------------
// Slicing helpers
// ---------------------------------------------------------------------------

/// Normalize slice bounds to a clamped `[a, b)` range.
fn slice_bounds(start: Option<i64>, end: Option<i64>, len: usize) -> (usize, usize) {
    let len_i = len as i64;
    let norm = |i: i64| {
        let v = if i < 0 { len_i + i } else { i };
        v.clamp(0, len_i)
    };
    let a = norm(start.unwrap_or(0));
    let b = norm(end.unwrap_or(len_i));
    if a > b {
        (0, 0)
    } else {
        (a as usize, b as usize)
    }
}

/// Slice an array or string: `s[1:3]`, `s[:2]`, `s[1:]`, `s[:]`.
/// Bounds are clamped; negative bounds count from the end.
pub(crate) fn slice_value(
    obj: &Value,
    start: Option<i64>,
    end: Option<i64>,
    span: Span,
) -> Result<Value, EvalError> {
    match obj {
        Value::Array(items) => {
            let (a, b) = slice_bounds(start, end, items.len());
            Ok(Value::Array(Box::new(items[a..b].to_vec())))
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let (a, b) = slice_bounds(start, end, chars.len());
            Ok(Value::Str(chars[a..b].iter().collect::<String>().into()))
        }
        other => Err(EvalError::new(
            format!("cannot slice a value of type `{}`", other.type_name()),
            span,
        )),
    }
}

// ---------------------------------------------------------------------------
// Arithmetic / unary helpers
// ---------------------------------------------------------------------------

/// Evaluate an integer binary operation.
///
/// In release builds, arithmetic uses wrapping semantics for speed.
/// In debug builds, checked operations catch overflow.
#[inline(always)]
fn eval_int_binary(op: BinOp, a: i64, b: i64, span: Span) -> Result<Value, EvalError> {
    match op {
        #[cfg(not(debug_assertions))]
        BinOp::Add => Ok(Value::Int(a.wrapping_add(b))),
        #[cfg(not(debug_assertions))]
        BinOp::Sub => Ok(Value::Int(a.wrapping_sub(b))),
        #[cfg(not(debug_assertions))]
        BinOp::Mul => Ok(Value::Int(a.wrapping_mul(b))),
        #[cfg(debug_assertions)]
        BinOp::Add => a
            .checked_add(b)
            .map(Value::Int)
            .ok_or_else(|| EvalError::new("integer overflow in addition", span)),
        #[cfg(debug_assertions)]
        BinOp::Sub => a
            .checked_sub(b)
            .map(Value::Int)
            .ok_or_else(|| EvalError::new("integer overflow in subtraction", span)),
        #[cfg(debug_assertions)]
        BinOp::Mul => a
            .checked_mul(b)
            .map(Value::Int)
            .ok_or_else(|| EvalError::new("integer overflow in multiplication", span)),
        BinOp::Div => {
            if b == 0 {
                Err(EvalError::new("division by zero", span))
            } else {
                #[cfg(not(debug_assertions))]
                {
                    Ok(Value::Int(a.wrapping_div(b)))
                }
                #[cfg(debug_assertions)]
                {
                    a.checked_div(b)
                        .map(Value::Int)
                        .ok_or_else(|| EvalError::new("integer overflow in division", span))
                }
            }
        }
        BinOp::Rem => {
            if b == 0 {
                Err(EvalError::new("modulo by zero", span))
            } else {
                #[cfg(not(debug_assertions))]
                {
                    Ok(Value::Int(a.wrapping_rem(b)))
                }
                #[cfg(debug_assertions)]
                {
                    a.checked_rem(b)
                        .map(Value::Int)
                        .ok_or_else(|| EvalError::new("integer overflow in modulo", span))
                }
            }
        }
        BinOp::Pow => {
            if b < 0 {
                Err(EvalError::new("negative exponent for integer power", span))
            } else {
                #[cfg(not(debug_assertions))]
                {
                    Ok(Value::Int(a.wrapping_pow(b as u32)))
                }
                #[cfg(debug_assertions)]
                {
                    a.checked_pow(b as u32)
                        .map(Value::Int)
                        .ok_or_else(|| EvalError::new("integer overflow in exponentiation", span))
                }
            }
        }
        BinOp::Eq => Ok(Value::Bool(a == b)),
        BinOp::Ne => Ok(Value::Bool(a != b)),
        BinOp::Lt => Ok(Value::Bool(a < b)),
        BinOp::Gt => Ok(Value::Bool(a > b)),
        BinOp::Le => Ok(Value::Bool(a <= b)),
        BinOp::Ge => Ok(Value::Bool(a >= b)),
        BinOp::And | BinOp::Or | BinOp::Elvis => unreachable!("short-circuited in eval"),
    }
}

/// Evaluate a binary operation, promoting mixed int/float to float.
#[inline(always)]
pub(crate) fn eval_binary(op: BinOp, l: Value, r: Value, span: Span) -> Result<Value, EvalError> {
    // Mixed int/float arithmetic promotes to float.
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => eval_int_binary(op, a, b, span),
        (Value::Float(a), Value::Float(b)) => match op {
            BinOp::Add => Ok(Value::Float(a + b)),
            BinOp::Sub => Ok(Value::Float(a - b)),
            BinOp::Mul => Ok(Value::Float(a * b)),
            BinOp::Div => Ok(Value::Float(a / b)),
            BinOp::Rem => Ok(Value::Float(a % b)),
            BinOp::Pow => Ok(Value::Float(a.powf(b))),
            BinOp::Eq => Ok(Value::Bool(a == b)),
            BinOp::Ne => Ok(Value::Bool(a != b)),
            BinOp::Lt => Ok(Value::Bool(a < b)),
            BinOp::Gt => Ok(Value::Bool(a > b)),
            BinOp::Le => Ok(Value::Bool(a <= b)),
            BinOp::Ge => Ok(Value::Bool(a >= b)),
            _ => Err(EvalError::new(
                format!("operator `{}` is not supported for floats", op.symbol()),
                span,
            )),
        },
        (Value::Str(a), Value::Str(b)) => match op {
            BinOp::Add => Ok(Value::Str(format!("{a}{b}").into())),
            BinOp::Eq => Ok(Value::Bool(a == b)),
            BinOp::Ne => Ok(Value::Bool(a != b)),
            BinOp::Lt => Ok(Value::Bool(a < b)),
            BinOp::Gt => Ok(Value::Bool(a > b)),
            BinOp::Le => Ok(Value::Bool(a <= b)),
            BinOp::Ge => Ok(Value::Bool(a >= b)),
            _ => Err(EvalError::new(
                format!("operator `{}` is not supported for strings", op.symbol()),
                span,
            )),
        },
        (l, r) => {
            let (a, b) = match (l.to_float(), r.to_float()) {
                (Some(a), Some(b)) => (a, b),
                _ => return Err(EvalError::new("arithmetic on non-numeric value", span)),
            };
            match op {
                BinOp::Add => Ok(Value::Float(a + b)),
                BinOp::Sub => Ok(Value::Float(a - b)),
                BinOp::Mul => Ok(Value::Float(a * b)),
                BinOp::Div => Ok(Value::Float(a / b)),
                BinOp::Rem => Ok(Value::Float(a % b)),
                BinOp::Pow => Ok(Value::Float(a.powf(b))),
                _ => Err(EvalError::new("arithmetic on non-numeric value", span)),
            }
        }
    }
}

/// Evaluate a unary operation.
#[inline(always)]
pub(crate) fn eval_unary(op: UnOp, v: Value, span: Span) -> Result<Value, EvalError> {
    match op {
        UnOp::Pos => Ok(v),
        UnOp::Neg => match v {
            Value::Int(i) => i
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| EvalError::new("integer overflow in negation", span)),
            Value::Float(f) => Ok(Value::Float(-f)),
            other => Err(EvalError::new(format!("cannot negate `{other}`"), span)),
        },
        UnOp::Not => match v {
            Value::Bool(b) => Ok(Value::Bool(!b)),
            other => Err(EvalError::new(
                format!("cannot apply `!` to `{other}`"),
                span,
            )),
        },
    }
}
