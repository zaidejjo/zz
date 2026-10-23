//! Shared operations on runtime values, used by both the tree-walker
//! interpreter and the bytecode VM.
//!
//! All functions here are `pub(crate)` standalone functions — they do NOT
//! take `&mut self` and can be called from any module in the crate.

use zz_frontend::ast::{BinOp, UnOp};
use zz_frontend::span::Span;

use std::collections::HashMap;
use std::sync::Arc;

use super::EvalError;
use crate::value::{DictData, ObjectValue, Value};

/// Interned one-byte ASCII strings, indexed by the byte value.
///
/// `s[i]` on an ASCII string used to build a fresh `String` *and* a fresh
/// `Arc` per character, which is what `str_scan` (176 k reads) paid for.
/// The table is filled once per process and a read is then a refcount bump.
fn ascii_chars() -> &'static [std::sync::Arc<String>; 128] {
    static TABLE: std::sync::OnceLock<[std::sync::Arc<String>; 128]> = std::sync::OnceLock::new();
    TABLE
        .get_or_init(|| std::array::from_fn(|b| std::sync::Arc::new((b as u8 as char).to_string())))
}

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
    given: &[(Arc<str>, Value)],
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
    let leftovers: Vec<(Arc<str>, Value)> = given
        .iter()
        .filter(|(n, _)| !registered.iter().any(|r| r.as_str() == n.as_ref()))
        .cloned()
        .collect();
    let mut out: Vec<(Arc<str>, Value)> = Vec::with_capacity(registered.len());
    for fname in registered {
        if let Some((name, v)) = given.iter().find(|(n, _)| n.as_ref() == fname.as_str()) {
            out.push((Arc::clone(name), v.clone()));
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
            out.push((
                Arc::from(fname.as_str()),
                Value::Object(std::sync::Arc::new(inner)),
            ));
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
    match object_field_opt(obj, name) {
        Some(v) => Ok(v),
        None => Err(object_field_miss(obj, name, span)),
    }
}

/// [`object_field`] without the miss error.
///
/// Method dispatch misses by construction — a method is a function-table
/// entry (`Type.method`), not a struct field — so `CallMethod` probes with
/// this instead: formatting ``struct `X` has no field `m` `` plus an
/// `EvalError` on every call, only to drop it, was a sizeable slice of the
/// per-call cost.
pub(crate) fn object_field_opt(obj: &Value, name: &str) -> Option<Value> {
    object_field_depth_opt(obj, name, 0)
}

fn object_field_depth_opt(obj: &Value, name: &str, depth: usize) -> Option<Value> {
    match obj {
        Value::Object(o) => {
            if let Some((_, v)) = o.fields.iter().find(|(n, _)| &**n == name) {
                return Some(v.clone());
            }
            if depth < MAX_EMBED_DEPTH {
                for (fname, v) in &o.fields {
                    if is_embedded_value(fname, v) {
                        if let Some(promoted) = object_field_depth_opt(v, name, depth + 1) {
                            return Some(promoted);
                        }
                    }
                }
            }
            None
        }
        Value::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, Value::Str(s) if s.as_str() == name))
            .map(|(_, v)| v.clone()),
        Value::HttpRequest(req) => match name {
            "method" => Some(Value::Str(req.method.clone().into())),
            "path" => Some(Value::Str(req.path.clone().into())),
            "body" => Some(Value::Str(req.body.clone().into())),
            "headers" | "query" | "params" => {
                let pairs = match name {
                    "headers" => &req.headers,
                    "query" => &req.query,
                    _ => &req.params,
                };
                Some(Value::Dict(std::sync::Arc::new(
                    pairs
                        .iter()
                        .map(|(k, v)| (Value::Str(k.clone().into()), Value::Str(v.clone().into())))
                        .collect(),
                )))
            }
            _ => None,
        },
        _ => None,
    }
}

/// The miss error for [`object_field`], kept off the probe path: the
/// message (and its `format!`) is only built when a real field read fails.
fn object_field_miss(obj: &Value, name: &str, span: Span) -> EvalError {
    match obj {
        Value::Object(o) => {
            EvalError::new(format!("struct `{}` has no field `{name}`", o.name), span)
        }
        Value::Dict(_) => EvalError::new(format!("dict has no key `{name}`"), span),
        Value::HttpRequest(_) => {
            EvalError::new(format!("http.request has no field `{name}`"), span)
        }
        other => EvalError::new(
            format!("cannot access field `{name}` on a value of type `{other}`"),
            span,
        ),
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
            // Copy-on-write: uniquely-owned objects mutate in place,
            // shared ones copy once here.
            let o = std::sync::Arc::make_mut(o);
            if let Some((_, slot)) = o.fields.iter_mut().find(|(n, _)| &**n == name) {
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

/// Fill omitted trailing default args for natives with `has_default`
/// checker signatures (see `sig_defaults` in `zz_stdlib`). Covers both the
/// canonical `std.http.*` keys and the short `http.*` spellings created by
/// `import std.http` namespace registration. Returns true when it filled
/// (all missing trailing params); false leaves `args` untouched so the
/// caller reports the arity error.
pub(crate) fn fill_default_headers(name: &str, args: &mut Vec<Value>, arity: usize) -> bool {
    const OPTIONAL_HEADERS: &[&str] = &[
        "std.http.get",
        "std.http.post",
        "std.http.put",
        "std.http.delete",
        "std.http.respond",
        "http.get",
        "http.post",
        "http.put",
        "http.delete",
        "http.respond",
        "std.http.post_json",
        "http.post_json",
    ];
    if OPTIONAL_HEADERS.contains(&name) && args.len() + 1 == arity {
        args.push(Value::Dict(std::sync::Arc::new(DictData::new())));
        return true;
    }
    // Unified client defaults, in signature order:
    // (url, method="GET", headers={}, body="", timeout_ms=30000).
    const FETCH: &[&str] = &[
        "std.http.fetch",
        "http.fetch",
        "std.http.fetch_insecure",
        "http.fetch_insecure",
    ];
    if FETCH.contains(&name) && args.len() < arity && args.len() >= arity - 4 {
        let missing = arity - args.len();
        // Defaults from the tail: timeout_ms, body, headers, method.
        let mut tail: Vec<Value> = Vec::with_capacity(missing);
        if missing >= 1 {
            tail.push(Value::Int(30000));
        }
        if missing >= 2 {
            tail.push(Value::Str(String::new().into()));
        }
        if missing >= 3 {
            tail.push(Value::Dict(std::sync::Arc::new(DictData::new())));
        }
        if missing >= 4 {
            tail.push(Value::Str("GET".to_string().into()));
        }
        tail.reverse();
        args.extend(tail);
        return true;
    }
    false
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

/// Read an element: `arr[i]`, `dict[key]`, `str[i]`, `bytes[i]`.
/// Negative indices count from the end.
#[inline(always)]
pub(crate) fn get_index(obj: &Value, index: &Value, span: Span) -> Result<Value, EvalError> {
    match (obj, index) {
        (Value::Array(items), Value::Int(i)) => {
            let idx = normalize_index(*i, items.len(), span)?;
            Ok(items[idx].clone())
        }
        (Value::Tuple(items), Value::Int(i)) => {
            let idx = normalize_index(*i, items.len(), span)?;
            Ok(items[idx].clone())
        }
        (Value::Bytes(b), Value::Int(i)) => {
            let idx = normalize_index(*i, b.len(), span)?;
            Ok(Value::Int(b.as_slice()[idx] as i64))
        }
        (Value::Dict(dict), key) => dict
            .get_cloned(key)
            .ok_or_else(|| EvalError::new(format!("key `{key}` not found in dict"), span)),
        (Value::Str(s), Value::Int(i)) => {
            // ASCII fast path: byte index == char index, no `Vec<char>`
            // allocation, and the single-character value comes from the
            // interned table. Non-ASCII keeps the exact char semantics below.
            if s.is_ascii() {
                let idx = normalize_index(*i, s.len(), span)?;
                let byte = s.as_bytes()[idx] as usize;
                Ok(Value::Str(std::sync::Arc::clone(&ascii_chars()[byte])))
            } else {
                let chars: Vec<char> = s.chars().collect();
                let idx = normalize_index(*i, chars.len(), span)?;
                Ok(Value::Str(chars[idx].to_string().into()))
            }
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
            // Copy-on-write: uniquely-owned buffers mutate in place,
            // shared ones copy once here (the copy value semantics
            // always required).
            std::sync::Arc::make_mut(items)[idx] = value;
            Ok(())
        }
        (Value::Tuple(items), Value::Int(i)) => {
            // Tuples share the array representation at runtime.
            let idx = normalize_index(*i, items.len(), span)?;
            std::sync::Arc::make_mut(items)[idx] = value;
            Ok(())
        }
        (Value::Dict(dict), key) => {
            // Copy-on-write: uniquely-owned dicts mutate in place, shared
            // ones copy once here (same value semantics as before); the
            // index is maintained by `insert`.
            let dict = std::sync::Arc::make_mut(dict);
            dict.insert(key.clone(), value);
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

/// Slice an array, byte buffer, or string: `s[1:3]`, `s[:2]`, `s[1:]`,
/// `s[:]`. Bounds are clamped; negative bounds count from the end.
/// Byte slices share the backing store (O(1), zero-copy).
pub(crate) fn slice_value(
    obj: &Value,
    start: Option<i64>,
    end: Option<i64>,
    span: Span,
) -> Result<Value, EvalError> {
    match obj {
        Value::Array(items) => {
            let (a, b) = slice_bounds(start, end, items.len());
            Ok(Value::Array(std::sync::Arc::new(items[a..b].to_vec())))
        }
        Value::Bytes(b) => {
            let (a, c) = slice_bounds(start, end, b.len());
            Ok(Value::Bytes(Box::new(b.slice(a, c))))
        }
        Value::Str(s) => {
            // ASCII fast path: byte bounds == char bounds (every byte is a
            // char boundary), so slice without the `Vec<char>` round-trip.
            if s.is_ascii() {
                let (a, b) = slice_bounds(start, end, s.len());
                Ok(Value::Str(s.get(a..b).unwrap_or("").to_string().into()))
            } else {
                let chars: Vec<char> = s.chars().collect();
                let (a, b) = slice_bounds(start, end, chars.len());
                Ok(Value::Str(chars[a..b].iter().collect::<String>().into()))
            }
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
/// Wrapping (two's-complement) semantics in every profile, per the IR
/// spec: `+ - *` never trap. Division/remainder keep their explicit
/// zero and `MIN` checks below.
#[inline(always)]
pub(crate) fn eval_int_binary(op: BinOp, a: i64, b: i64, span: Span) -> Result<Value, EvalError> {
    match op {
        BinOp::Add => Ok(Value::Int(a.wrapping_add(b))),
        BinOp::Sub => Ok(Value::Int(a.wrapping_sub(b))),
        BinOp::Mul => Ok(Value::Int(a.wrapping_mul(b))),
        BinOp::Div => {
            if b == 0 {
                Err(EvalError::new("division by zero", span))
            } else if a == i64::MIN && b == -1 {
                // Traps in every profile (IR spec); -fwrapv does not
                // save division, and wrapping_div would hide it.
                Err(EvalError::new("integer overflow in division", span))
            } else {
                Ok(Value::Int(a.wrapping_div(b)))
            }
        }
        BinOp::Rem => {
            if b == 0 {
                Err(EvalError::new("modulo by zero", span))
            } else if a == i64::MIN && b == -1 {
                // Traps in every profile (IR spec).
                Err(EvalError::new("integer overflow in modulo", span))
            } else {
                Ok(Value::Int(a.wrapping_rem(b)))
            }
        }
        BinOp::Pow => {
            if b < 0 {
                Err(EvalError::new("negative exponent for integer power", span))
            } else {
                // Wrapping power in every profile (IR spec).
                Ok(Value::Int(a.wrapping_pow(b as u32)))
            }
        }
        // Bitwise ops never overflow — wrapping in all build modes,
        // like `+`/`-`/`*` since the wrap-spec change. Shifts mask to
        // `b & 63` (x86 / Rust `wrapping_shl` semantics) so large
        // counts can't UB; negative counts are a runtime error.
        BinOp::BitAnd => Ok(Value::Int(a & b)),
        BinOp::BitOr => Ok(Value::Int(a | b)),
        BinOp::BitXor => Ok(Value::Int(a ^ b)),
        BinOp::Shl => {
            if b < 0 {
                Err(EvalError::new(
                    format!("negative shift count {b} for `<<`"),
                    span,
                ))
            } else {
                Ok(Value::Int(
                    ((a as u64).wrapping_shl((b as u64 & 63) as u32)) as i64,
                ))
            }
        }
        BinOp::Shr => {
            if b < 0 {
                Err(EvalError::new(
                    format!("negative shift count {b} for `>>`"),
                    span,
                ))
            } else {
                // Arithmetic (sign-extending) right shift.
                Ok(Value::Int(a.wrapping_shr((b & 63) as u32)))
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
            // Wrapping negation (IR spec): -MIN is MIN, never a trap.
            Value::Int(i) => Ok(Value::Int(i.wrapping_neg())),
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
        UnOp::BitNot => match v {
            Value::Int(i) => Ok(Value::Int(!i)),
            other => Err(EvalError::new(
                format!("cannot apply `~` to `{other}`: bitwise NOT requires `int`"),
                span,
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{get_index, object_field, object_field_opt};
    use crate::value::{ObjectValue, Value};
    use zz_frontend::span::Span;

    /// `s[i]` on an ASCII string hands out interned one-character values, so
    /// repeated reads are a refcount bump (the `str_scan` cost). Non-ASCII
    /// keeps char semantics: byte index != char index there.
    #[test]
    fn ascii_char_reads_are_interned() {
        let span = Span::new(0, 0);
        let s = Value::Str("ab".to_string().into());
        let first = get_index(&s, &Value::Int(0), span).unwrap();
        let again = get_index(&s, &Value::Int(0), span).unwrap();
        assert_eq!(first, Value::Str("a".to_string().into()));
        match (first, again) {
            (Value::Str(a), Value::Str(b)) => assert!(std::sync::Arc::ptr_eq(&a, &b)),
            _ => panic!("expected string values"),
        }
        let unicode = Value::Str("héllo".to_string().into());
        assert_eq!(
            get_index(&unicode, &Value::Int(1), span).unwrap(),
            Value::Str("é".to_string().into())
        );
        assert_eq!(
            get_index(&unicode, &Value::Int(4), span).unwrap(),
            Value::Str("o".to_string().into())
        );
    }

    /// `CallMethod` probes with [`object_field_opt`] and only the real read
    /// path formats [`object_field`]'s error, so both must agree on hits and
    /// the error text must not drift.
    #[test]
    fn field_probe_matches_the_error_path() {
        let span = Span::new(0, 0);
        let obj = Value::Object(std::sync::Arc::new(ObjectValue {
            name: "P".into(),
            fields: vec![("x".into(), Value::Int(1))],
        }));
        assert_eq!(object_field_opt(&obj, "x"), Some(Value::Int(1)));
        assert_eq!(object_field_opt(&obj, "nope"), None);
        assert_eq!(
            object_field(&obj, "nope", span).unwrap_err().message,
            "struct `P` has no field `nope`"
        );

        let dict = Value::Dict(std::sync::Arc::new(crate::value::DictData::from_entries(
            vec![(
                Value::Str(std::sync::Arc::new("k".to_string())),
                Value::Int(2),
            )],
        )));
        assert_eq!(object_field_opt(&dict, "k"), Some(Value::Int(2)));
        assert_eq!(object_field_opt(&dict, "missing"), None);
        assert_eq!(
            object_field(&dict, "missing", span).unwrap_err().message,
            "dict has no key `missing`"
        );

        let int = Value::Int(3);
        assert_eq!(object_field_opt(&int, "f"), None);
        assert_eq!(
            object_field(&int, "f", span).unwrap_err().message,
            "cannot access field `f` on a value of type `3`"
        );
    }
}
