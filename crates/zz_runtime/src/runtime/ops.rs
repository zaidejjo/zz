//! Shared operations on runtime values, used by both the tree-walker
//! interpreter and the bytecode VM.
//!
//! All functions here are `pub(crate)` standalone functions — they do NOT
//! take `&mut self` and can be called from any module in the crate.

use zz_frontend::ast::{BinOp, UnOp};
use zz_frontend::span::Span;

use std::collections::HashMap;

use super::EvalError;
use crate::value::{ObjectValue, Value};

// ---------------------------------------------------------------------------
// Object / field helpers
// ---------------------------------------------------------------------------

/// True when a struct slot holds an embedded (anonymous) field value: an
/// object whose type's last segment equals the field name
/// (`User.Base: Base{...}`). Mirrors the checker's `is_embedded_field`,
/// but value-based so both interpreter engines share it without layouts.
#[inline]
pub(crate) fn is_embedded_value(fname: &str, value: &Value) -> bool {
    match value {
        Value::Object(o) => o.name.rsplit('.').next().unwrap_or(&o.name) == fname,
        _ => false,
    }
}

/// Resolve the layout name of an embedded field: the field name itself
/// (`Base`) or the single namespaced struct whose last segment matches
/// (`mod.Base`). Sorted-first for determinism when several match.
pub(crate) fn embedded_layout_name(
    layouts: &HashMap<String, Vec<String>>,
    fname: &str,
) -> Option<String> {
    if layouts.contains_key(fname) {
        return Some(fname.to_string());
    }
    let suffix = format!(".{fname}");
    let mut hits: Vec<&String> = layouts.keys().filter(|k| k.ends_with(&suffix)).collect();
    hits.sort();
    hits.into_iter().next().cloned()
}

/// Build a struct value from already-evaluated literal fields, distributing
/// flattened (promoted) fields into embedded sub-objects: `User{id: 1, age:
/// 2}` fills `Base.id` from the leftover `id`. `given` holds the literal's
/// `(name, value)` pairs; `registered` is the struct's own field layout.
/// Used by the bytecode VM's `MakeStruct`.
pub(crate) fn build_struct_literal(
    layouts: &HashMap<String, Vec<String>>,
    sname: &str,
    registered: &[String],
    given: &[(String, Value)],
    span: Span,
    depth: usize,
) -> Result<ObjectValue, EvalError> {
    if depth > MAX_EMBED_DEPTH {
        return Err(EvalError::new(
            format!("struct `{sname}` is embedded too deeply (possible cycle)"),
            span,
        ));
    }
    // Leftovers: given fields that are not direct fields of this struct —
    // candidates for embedded sub-objects.
    let leftovers: Vec<(String, Value)> = given
        .iter()
        .filter(|(n, _)| !registered.contains(n))
        .cloned()
        .collect();
    let mut out = Vec::with_capacity(registered.len());
    for fname in registered {
        if let Some((_, v)) = given.iter().find(|(n, _)| n == fname) {
            out.push((fname.clone(), v.clone()));
        } else if let Some(inner_name) = embedded_layout_name(layouts, fname) {
            let Some(inner_layout) = layouts.get(&inner_name).cloned() else {
                return Err(EvalError::new(
                    format!("unknown struct `{inner_name}`"),
                    span,
                ));
            };
            let inner = build_struct_literal(
                layouts,
                &inner_name,
                &inner_layout,
                &leftovers,
                span,
                depth + 1,
            )?;
            out.push((fname.clone(), Value::Object(Box::new(inner))));
        } else {
            return Err(EvalError::new(
                format!("missing field `{fname}` in struct literal"),
                span,
            ));
        }
    }
    Ok(ObjectValue {
        name: sname.to_string(),
        fields: out,
    })
}

/// Maximum promotion depth when searching embedded structs. Struct values
/// are finite trees, so this is only a safety bound.
const MAX_EMBED_DEPTH: usize = 32;

/// Read a field from a struct instance, promoting through embedded
/// (anonymous) fields transitively: `u.id` finds `u.Base.id`.
#[inline(always)]
pub(crate) fn object_field(obj: &Value, name: &str, span: Span) -> Result<Value, EvalError> {
    object_field_depth(obj, name, span, 0)
}

fn object_field_depth(
    obj: &Value,
    name: &str,
    span: Span,
    depth: usize,
) -> Result<Value, EvalError> {
    match obj {
        Value::Object(o) => {
            if let Some((_, v)) = o.fields.iter().find(|(n, _)| n == name) {
                return Ok(v.clone());
            }
            if depth < MAX_EMBED_DEPTH {
                for (fname, v) in &o.fields {
                    if is_embedded_value(fname, v) {
                        if let Ok(promoted) = object_field_depth(v, name, span, depth + 1) {
                            return Ok(promoted);
                        }
                    }
                }
            }
            Err(EvalError::new(
                format!("struct `{}` has no field `{name}`", o.name),
                span,
            ))
        }
        Value::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, Value::Str(s) if s.as_str() == name))
            .map(|(_, v)| v.clone())
            .ok_or_else(|| EvalError::new(format!("dict has no key `{name}`"), span)),
        other => Err(EvalError::new(
            format!("cannot access field `{name}` on a value of type `{other}`"),
            span,
        )),
    }
}

/// Write a field into a struct instance (in place), promoting through
/// embedded fields: `u.id = 1` writes `u.Base.id`.
pub(crate) fn set_object_field(
    obj: &mut Value,
    name: &str,
    value: Value,
    span: Span,
) -> Result<(), EvalError> {
    set_object_field_depth(obj, name, value, span, 0)
}

fn set_object_field_depth(
    obj: &mut Value,
    name: &str,
    value: Value,
    span: Span,
    depth: usize,
) -> Result<(), EvalError> {
    match obj {
        Value::Object(o) => {
            if let Some((_, slot)) = o.fields.iter_mut().find(|(n, _)| n == name) {
                *slot = value;
                Ok(())
            } else if depth < MAX_EMBED_DEPTH {
                // `o.name` is needed for the error below; copy it out so
                // the embedded recursion can mutably borrow the fields.
                let type_name = o.name.clone();
                for (fname, v) in o.fields.iter_mut() {
                    if is_embedded_value(fname, v)
                        && set_object_field_depth(v, name, value.clone(), span, depth + 1).is_ok()
                    {
                        return Ok(());
                    }
                }
                Err(EvalError::new(
                    format!("struct `{type_name}` has no field `{name}`"),
                    span,
                ))
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
pub(crate) fn eval_int_binary(op: BinOp, a: i64, b: i64, span: Span) -> Result<Value, EvalError> {
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
///
/// Equality (`==` / `!=`) works for every value via [`Value`]'s `PartialEq`
/// (bools, units, arrays, dicts, options, structs, ...). Mismatched types
/// compare as unequal. Numeric `int`/`float` mixes compare as floats.
#[inline(always)]
pub(crate) fn eval_binary(op: BinOp, l: Value, r: Value, span: Span) -> Result<Value, EvalError> {
    // Equality first: covers Bool and every other non-numeric (or
    // mismatched) pair that the typed arms below would otherwise reject
    // with a misleading "arithmetic on non-numeric value" error.
    match op {
        BinOp::Eq | BinOp::Ne => {
            // Exact integer equality (no float rounding for large i64).
            if let (Value::Int(a), Value::Int(b)) = (&l, &r) {
                let eq = a == b;
                return Ok(Value::Bool(if op == BinOp::Eq { eq } else { !eq }));
            }
            if let (Some(a), Some(b)) = (l.to_float(), r.to_float()) {
                // Mixed int/float numerics: compare as floats.
                let eq = a == b;
                return Ok(Value::Bool(if op == BinOp::Eq { eq } else { !eq }));
            }
            let eq = l == r;
            return Ok(Value::Bool(if op == BinOp::Eq { eq } else { !eq }));
        }
        _ => {}
    }
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
            if let (Some(a), Some(b)) = (l.to_float(), r.to_float()) {
                return match op {
                    BinOp::Add => Ok(Value::Float(a + b)),
                    BinOp::Sub => Ok(Value::Float(a - b)),
                    BinOp::Mul => Ok(Value::Float(a * b)),
                    BinOp::Div => Ok(Value::Float(a / b)),
                    BinOp::Rem => Ok(Value::Float(a % b)),
                    BinOp::Pow => Ok(Value::Float(a.powf(b))),
                    BinOp::Lt => Ok(Value::Bool(a < b)),
                    BinOp::Gt => Ok(Value::Bool(a > b)),
                    BinOp::Le => Ok(Value::Bool(a <= b)),
                    BinOp::Ge => Ok(Value::Bool(a >= b)),
                    // `==` / `!=` return early above; `&&` / `||` short-circuit
                    // in eval. Unreachable here.
                    _ => Err(EvalError::new("arithmetic on non-numeric value", span)),
                };
            }
            match op {
                BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => Err(EvalError::new(
                    format!(
                        "operator `{}` is not supported for {} and {}",
                        op.symbol(),
                        l.type_name(),
                        r.type_name()
                    ),
                    span,
                )),
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
