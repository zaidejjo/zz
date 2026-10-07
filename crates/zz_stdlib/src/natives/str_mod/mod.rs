use crate::natives::{arg, expect_array, expect_int, expect_str};
use zz_runtime::{EvalError, Interp, Span, Value};

pub(crate) fn str_length(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.length")?;
    Ok(Value::Int(s.chars().count() as i64))
}

pub(crate) fn str_split(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.split")?;
    let sep = expect_str(args, 1, "std.str.split")?;
    let parts: Vec<Value> = s
        .split(&sep)
        .map(|p| Value::Str(p.to_string().into()))
        .collect();
    Ok(Value::Array(Box::new(parts)))
}

pub(crate) fn str_contains(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.contains")?;
    let sub = expect_str(args, 1, "std.str.contains")?;
    Ok(Value::Bool(s.contains(&sub)))
}

pub(crate) fn str_find(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.find")?;
    let sub = expect_str(args, 1, "std.str.find")?;
    let from = expect_int(args, 2, "std.str.find")?;
    Ok(Value::Int(byte_find(&s, &sub, from)))
}

pub(crate) fn str_rfind(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.rfind")?;
    let sub = expect_str(args, 1, "std.str.rfind")?;
    let from = expect_int(args, 2, "std.str.rfind")?;
    Ok(Value::Int(byte_rfind(&s, &sub, from)))
}

// Byte offsets throughout: O(1) per call, O(n) streaming total, and
// backend-identical on every input (no char-counting divergence on
// invalid sequences). Matches Rust str::find/rfind byte semantics.
// `len()`/slicing stay char-oriented; convert explicitly when mixing.
// Empty `sub` returns clamped `from`; negatives clamp to 0.
fn byte_find(s: &str, sub: &str, from: i64) -> i64 {
    let bytes = s.as_bytes();
    let n = bytes.len() as i64;
    let mut start = from.clamp(0, n) as usize;
    while start < bytes.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    if sub.is_empty() {
        return start.min(bytes.len()) as i64;
    }
    if start >= bytes.len() {
        return -1;
    }
    match s[start..].find(sub) {
        Some(rel) => start as i64 + rel as i64,
        None => -1,
    }
}

fn byte_rfind(s: &str, sub: &str, from: i64) -> i64 {
    let bytes = s.as_bytes();
    let n = bytes.len() as i64;
    let mut end = from.clamp(0, n) as usize;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    if sub.is_empty() {
        return end as i64;
    }
    // A match starting exactly at `end` may extend past it.
    let mut window = end + sub.len();
    if window > bytes.len() {
        window = bytes.len();
    }
    while window > end && !s.is_char_boundary(window) {
        window -= 1;
    }
    let mut best: Option<i64> = s[..window].rfind(sub).map(|b| b as i64);
    if s.is_char_boundary(end) && end + sub.len() <= bytes.len() && s[end..].starts_with(sub) {
        best = Some(match best {
            Some(prev) => prev.max(end as i64),
            None => end as i64,
        });
    }
    best.unwrap_or(-1)
}

// str.bytes(s) — UTF-8 bytes as plain ints. One O(n) copy; the
// result composes with every [int] API (indexing, snapshots, the
// bytes.* builder vocabulary). The bridge find/rfind/trim_span need.
pub(crate) fn str_bytes(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.bytes")?;
    Ok(Value::Array(Box::new(
        s.as_bytes().iter().map(|b| Value::Int(*b as i64)).collect(),
    )))
}

// bytes.to_str(vs) — strict UTF-8 decode; invalid sequences and
// out-of-range values are .err (identical on VM and AOT).
pub(crate) fn bytes_to_str(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "bytes.to_str")?;
    let mut buf = Vec::with_capacity(vs.len());
    for v in &vs {
        match v {
            Value::Int(n) => {
                if !(0..=255).contains(n) {
                    return Ok(Value::Result(Box::new(Err(Value::Str(Box::new(format!(
                        "bytes.to_str: value {n} out of range 0-255"
                    )))))));
                }
                buf.push(*n as u8);
            }
            other => {
                return Err(EvalError::new(
                    format!("`bytes.to_str` expects an array of integers, found `{other}`"),
                    zz_runtime::Span::new(0, 0),
                ));
            }
        }
    }
    match String::from_utf8(buf) {
        Ok(s) => Ok(Value::Result(Box::new(Ok(Value::Str(Box::new(s)))))),
        Err(_) => Ok(Value::Result(Box::new(Err(Value::Str(Box::new(
            "bytes.to_str: invalid UTF-8".to_string(),
        )))))),
    }
}

// bytes.to_ints(b) — opaque byte buffer as plain ints (zero-copy read).
pub(crate) fn bytes_to_ints(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    match super::arg(args, 0, "bytes.to_ints")? {
        Value::Bytes(b) => Ok(Value::Array(Box::new(
            b.as_slice().iter().map(|x| Value::Int(*x as i64)).collect(),
        ))),
        other => Err(EvalError::new(
            format!("`bytes.to_ints` expects bytes, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn str_starts_with_at(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.starts_with_at")?;
    let sub = expect_str(args, 1, "std.str.starts_with_at")?;
    let pos = expect_int(args, 2, "std.str.starts_with_at")?;
    Ok(Value::Bool(starts_at(&s, &sub, pos)))
}

pub(crate) fn str_ends_with_at(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.ends_with_at")?;
    let sub = expect_str(args, 1, "std.str.ends_with_at")?;
    let pos = expect_int(args, 2, "std.str.ends_with_at")?;
    Ok(Value::Bool(ends_at(&s, &sub, pos)))
}

pub(crate) fn str_trim_span(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.trim_span")?;
    let start = expect_int(args, 1, "std.str.trim_span")?;
    let end = expect_int(args, 2, "std.str.trim_span")?;
    let (ns, ne) = trim_span(&s, start, end);
    Ok(Value::Array(Box::new(vec![Value::Int(ns), Value::Int(ne)])))
}

fn starts_at(s: &str, sub: &str, pos: i64) -> bool {
    if sub.is_empty() {
        return false;
    }
    let bytes = s.as_bytes();
    if pos < 0 || pos as usize + sub.len() > bytes.len() {
        return false;
    }
    let base = pos as usize;
    // Floor: a match may only start at a char boundary, and the window
    // must end at one too (slicing panics mid-char).
    if !s.is_char_boundary(base) || !s.is_char_boundary(base + sub.len()) {
        return false;
    }
    &s[base..base + sub.len()] == sub
}

// True when `sub` ends at byte offset `pos` (exclusive end).
fn ends_at(s: &str, sub: &str, pos: i64) -> bool {
    if sub.is_empty() {
        return false;
    }
    let bytes = s.as_bytes();
    if pos < 0 || pos as usize > bytes.len() || (pos as usize) < sub.len() {
        return false;
    }
    let base = (pos as usize) - sub.len();
    if !s.is_char_boundary(base) || !s.is_char_boundary(pos as usize) {
        return false;
    }
    &s[base..pos as usize] == sub
}

// Trimmed span of s[start..end] as byte offsets. Unicode White_Space on
// BOTH backends (explicit table, not the host trim): new API, no
// back-compat baggage, and backend-identical by construction.
fn trim_span(s: &str, start: i64, end: i64) -> (i64, i64) {
    let bytes = s.as_bytes();
    let n = bytes.len() as i64;
    let mut lo = start.clamp(0, n);
    let hi0 = end.clamp(0, n);
    let mut hi = hi0.max(lo);
    while lo < hi {
        let w = ws_width(bytes, lo as usize, hi as usize);
        if w == 0 {
            break;
        }
        lo += w as i64;
    }
    while hi > lo {
        let w = ws_width_back(bytes, lo as usize, hi as usize);
        if w == 0 {
            break;
        }
        hi -= w as i64;
    }
    (lo, hi)
}

// Width of whitespace at byte pos (0 if none): ASCII ws plus the
// Unicode White_Space sequences (NBSP, NEL, Ogham, En/Em quad,
// line/paragraph separators, narrow nbsp, medium space, ideographic).
fn ws_width(b: &[u8], pos: usize, end: usize) -> usize {
    if pos >= end {
        return 0;
    }
    let c = b[pos];
    if c == 32 || c == 9 || c == 10 || c == 11 || c == 12 || c == 13 {
        return 1;
    }
    if c == 0xC2 && pos + 1 < end && (b[pos + 1] == 0x85 || b[pos + 1] == 0xA0) {
        return 2;
    }
    if c == 0xE1 && pos + 2 < end && b[pos + 1] == 0x9A && b[pos + 2] == 0x80 {
        return 3;
    }
    if c == 0xE2 && pos + 2 < end && b[pos + 1] == 0x80 {
        let d = b[pos + 2];
        if (0x80..=0x8A).contains(&d) || d == 0xA8 || d == 0xA9 || d == 0xAF {
            return 3;
        }
    }
    if c == 0xE2 && pos + 2 < end && b[pos + 1] == 0x81 && b[pos + 2] == 0x9F {
        return 3;
    }
    if c == 0xE3 && pos + 2 < end && b[pos + 1] == 0x80 && b[pos + 2] == 0x80 {
        return 3;
    }
    0
}

fn ws_width_back(b: &[u8], s: usize, end: usize) -> usize {
    if end <= s {
        return 0;
    }
    let c = b[end - 1];
    if c == 32 || c == 9 || c == 10 || c == 11 || c == 12 || c == 13 {
        return 1;
    }
    if end - 2 >= s && b[end - 2] == 0xC2 && (c == 0x85 || c == 0xA0) {
        return 2;
    }
    if end - 3 >= s && b[end - 3] == 0xE1 && b[end - 2] == 0x9A && c == 0x80 {
        return 3;
    }
    if end - 3 >= s
        && b[end - 3] == 0xE2
        && b[end - 2] == 0x80
        && ((0x80..=0x8A).contains(&c) || c == 0xA8 || c == 0xA9 || c == 0xAF)
    {
        return 3;
    }
    if end - 3 >= s && b[end - 3] == 0xE2 && b[end - 2] == 0x81 && c == 0x9F {
        return 3;
    }
    if end - 3 >= s && b[end - 3] == 0xE3 && b[end - 2] == 0x80 && c == 0x80 {
        return 3;
    }
    0
}

pub(crate) fn str_trim(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.trim")?;
    Ok(Value::Str(s.trim().to_string().into()))
}

pub(crate) fn str_to_upper(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.to_upper")?;
    Ok(Value::Str(s.to_uppercase().into()))
}

pub(crate) fn str_to_lower(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.to_lower")?;
    Ok(Value::Str(s.to_lowercase().into()))
}

pub(crate) fn str_replace(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.replace")?;
    let old = expect_str(args, 1, "str.replace")?;
    let new = expect_str(args, 2, "str.replace")?;
    Ok(Value::Str(s.replace(&old, &new).into()))
}

pub(crate) fn str_starts_with(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.starts_with")?;
    let prefix = expect_str(args, 1, "str.starts_with")?;
    Ok(Value::Bool(s.starts_with(&prefix)))
}

pub(crate) fn str_ends_with(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.ends_with")?;
    let suffix = expect_str(args, 1, "str.ends_with")?;
    Ok(Value::Bool(s.ends_with(&suffix)))
}

pub(crate) fn str_join(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let items = match arg(args, 0, "str.join")?.clone() {
        Value::Array(arr) => arr,
        other => {
            return Err(EvalError::new(
                format!("`str.join` expects an array, found `{other}`"),
                _span,
            ));
        }
    };
    let sep = expect_str(args, 1, "str.join")?;
    let strs: Vec<String> = items
        .iter()
        .map(|v| match v {
            Value::Str(s) => (**s).clone(),
            // Display semantics so Options unwrap instead of leaking wrappers.
            other => other.to_display_string(),
        })
        .collect();
    Ok(Value::Str(strs.join(&sep).into()))
}

pub(crate) fn str_trim_start(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.trim_start")?;
    Ok(Value::Str(s.trim_start().to_string().into()))
}

pub(crate) fn str_trim_end(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "str.trim_end")?;
    Ok(Value::Str(s.trim_end().to_string().into()))
}
