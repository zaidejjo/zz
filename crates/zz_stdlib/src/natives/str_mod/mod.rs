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
    let start = from.clamp(0, n) as usize;
    if sub.is_empty() {
        return start as i64;
    }
    if start >= bytes.len() {
        return -1;
    }
    match memchr::memmem::find(&bytes[start..], sub.as_bytes()) {
        Some(rel) => start as i64 + rel as i64,
        None => -1,
    }
}

fn byte_rfind(s: &str, sub: &str, from: i64) -> i64 {
    let bytes = s.as_bytes();
    let n = bytes.len() as i64;
    let end = from.clamp(0, n) as usize;
    if sub.is_empty() {
        return end as i64;
    }
    // Last match starting at/before `end`, plus the explicit end-start
    // check (it may extend past the window). Matches can only start at
    // char boundaries, so no snapping is needed for byte-exact results.
    let window = (end + sub.len()).min(bytes.len());
    let mut best: Option<i64> =
        memchr::memmem::rfind(&bytes[..window], sub.as_bytes()).map(|b| b as i64);
    if end + sub.len() <= bytes.len() && &bytes[end..end + sub.len()] == sub.as_bytes() {
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
// str.count(s, sub) — non-overlapping occurrences, no allocation.
// Edge semantics mirror the old split-based version exactly:
// empty sub counts chars(s)+1 (split inserts between every char plus
// both ends: "abc" -> 4, "" -> 1).
pub(crate) fn str_count(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.count")?;
    let sub = expect_str(args, 1, "std.str.count")?;
    if sub.is_empty() {
        return Ok(Value::Int(s.chars().count() as i64 + 1));
    }
    Ok(Value::Int(
        memchr::memmem::find_iter(s.as_bytes(), sub.as_bytes()).count() as i64,
    ))
}

// str.classify(text, markers, bstart, bend, nested, whole) —
// comment-aware line classification in one native call: returns
// [lines, code, comments, blanks]. Byte-oriented with ASCII-4 trim,
// mirroring the split/trim/starts_with native semantics exactly.
// `markers` is the line-comment list, `bstart`/`bend` the block pair
// ("" = none), `nested` enables Rust-style nesting depth, `whole`
// selects whole-line blocks (Ruby =begin / Perl =cut).
pub(crate) fn str_classify(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let text = expect_str(args, 0, "std.str.classify")?;
    let raw_markers = super::expect_array(args, 1, "std.str.classify")?;
    let bstart = expect_str(args, 2, "std.str.classify")?;
    let bend = expect_str(args, 3, "std.str.classify")?;
    let nested = match super::arg(args, 4, "std.str.classify")? {
        Value::Bool(b) => *b,
        other => {
            return Err(EvalError::new(
                format!("`std.str.classify` expects booleans, found `{other}`"),
                zz_runtime::Span::new(0, 0),
            ));
        }
    };
    let whole = match super::arg(args, 5, "std.str.classify")? {
        Value::Bool(b) => *b,
        other => {
            return Err(EvalError::new(
                format!("`std.str.classify` expects booleans, found `{other}`"),
                zz_runtime::Span::new(0, 0),
            ));
        }
    };
    let mut markers: Vec<&[u8]> = Vec::with_capacity(raw_markers.len());
    for m in &raw_markers {
        match m {
            Value::Str(s) => markers.push(s.as_bytes()),
            other => {
                return Err(EvalError::new(
                    format!("`std.str.classify` expects marker strings, found `{other}`"),
                    zz_runtime::Span::new(0, 0),
                ));
            }
        }
    }
    let (lines, code, comments, blanks) = classify_bytes(
        text.as_bytes(),
        &markers,
        bstart.as_bytes(),
        bend.as_bytes(),
        nested,
        whole,
    );
    Ok(Value::Array(Box::new(vec![
        Value::Int(lines),
        Value::Int(code),
        Value::Int(comments),
        Value::Int(blanks),
    ])))
}

fn is_btrim(b: u8) -> bool {
    b == 32 || b == 9 || b == 10 || b == 13
}

fn trim_span_b(line: &[u8]) -> (usize, usize) {
    let mut s = 0;
    let mut e = line.len();
    while s < e && is_btrim(line[s]) {
        s += 1;
    }
    while e > s && is_btrim(line[e - 1]) {
        e -= 1;
    }
    (s, e)
}

fn starts_with_any_b(line: &[u8], tls: usize, markers: &[&[u8]]) -> bool {
    for m in markers {
        if !m.is_empty() && line[tls..].starts_with(m) {
            return true;
        }
    }
    false
}

fn contains_b(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && memchr::memmem::find(hay, needle).is_some()
}

// Blank string spans for quote q (blank_quote semantics): backslash
// pairs `\\` and `\q` erased first, then toggle on q.
fn blank_q_b(buf: &mut [u8], q: u8) {
    let n = buf.len();
    let mut i = 0;
    let mut inside = false;
    while i < n {
        let c = buf[i];
        if c == 92 && i + 1 < n && (buf[i + 1] == 92 || buf[i + 1] == q) {
            buf[i] = 32;
            buf[i + 1] = 32;
            i += 2;
            continue;
        }
        if c == q {
            inside = !inside;
            i += 1;
            continue;
        }
        if inside {
            buf[i] = 32;
        }
        i += 1;
    }
}

fn needs_blank(line: &[u8], markers: &[&[u8]], has_open: bool) -> bool {
    if !has_open {
        let mut found = false;
        for m in markers {
            if !m.is_empty() && contains_b(line, m) {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }
    line.contains(&b'\"') || line.contains(&b'\'') || line.contains(&b'`')
}

fn classify_bytes(
    text: &[u8],
    markers: &[&[u8]],
    bstart: &[u8],
    bend: &[u8],
    nested: bool,
    whole: bool,
) -> (i64, i64, i64, i64) {
    let no_comment = markers.iter().all(|m| m.is_empty()) && bstart.is_empty();
    let mut lines = 0i64;
    let mut code = 0i64;
    let mut comments = 0i64;
    let mut blanks = 0i64;
    // Split on \n, drop one trailing artifact (mirrors split_lines).
    let mut parts: Vec<&[u8]> = text.split(|&c| c == 10).collect();
    if parts.last().is_some_and(|l| l.is_empty()) {
        parts.pop();
    }
    let mut in_block = false;
    let mut depth = 0i64;
    for raw in parts {
        lines += 1;
        if raw.is_empty() {
            blanks += 1;
            continue;
        }
        let (tls, the) = trim_span_b(raw);
        if tls >= the {
            blanks += 1;
            continue;
        }
        if no_comment {
            code += 1;
            continue;
        }
        if whole {
            if !bstart.is_empty() && raw.get(tls..the) == Some(bstart) {
                in_block = true;
                comments += 1;
                continue;
            }
            if !bend.is_empty() && raw.get(tls..the) == Some(bend) {
                in_block = false;
                comments += 1;
                continue;
            }
            if in_block {
                comments += 1;
                continue;
            }
            if starts_with_any_b(raw, tls, markers) {
                comments += 1;
            } else {
                code += 1;
            }
            continue;
        }
        if in_block {
            let mut owned: Vec<u8> = Vec::new();
            // Either gate (block-open present, or a line marker seen)
            // forces the same string blanking, so one condition covers
            // both — the bodies were identical by construction.
            let gate = (!bstart.is_empty()
                && memchr::memmem::find(raw, bstart).is_some()
                && needs_blank(raw, markers, true))
                || needs_blank(raw, markers, false);
            let cl: &[u8] = if gate {
                owned.extend_from_slice(raw);
                blank_q_b(&mut owned, b'"');
                blank_q_b(&mut owned, b'`');
                blank_q_b(&mut owned, b'\'');
                &owned
            } else {
                raw
            };
            if nested {
                let si = count_occ(cl, bstart);
                let ei = count_occ(cl, bend);
                depth += si - ei;
                if depth <= 0 {
                    in_block = false;
                    depth = 0;
                }
            } else if !bend.is_empty() && memchr::memmem::find(cl, bend).is_some() {
                in_block = false;
                let tail = match memchr::memmem::rfind(cl, bend) {
                    Some(bpos) => &cl[bpos + bend.len()..],
                    None => b"",
                };
                let (ttl, tth) = trim_span_b(tail);
                if ttl < tth && !starts_with_any_b(raw, tls, markers) {
                    code += 1;
                    continue;
                }
            }
            comments += 1;
            continue;
        }
        if starts_with_any_b(raw, tls, markers) {
            comments += 1;
            continue;
        }
        if !bstart.is_empty() && memchr::memmem::find(raw, bstart).is_some() {
            let mut owned: Vec<u8> = Vec::new();
            let cl: &[u8] = if needs_blank(raw, markers, true) {
                owned.extend_from_slice(raw);
                blank_q_b(&mut owned, b'"');
                blank_q_b(&mut owned, b'`');
                blank_q_b(&mut owned, b'\'');
                &owned
            } else {
                raw
            };
            if memchr::memmem::find(cl, bstart).is_none() {
                code += 1;
                continue;
            }
            let bi = memchr::memmem::find(cl, bstart).unwrap_or(cl.len());
            let (bl, bh) = trim_span_b(&cl[..bi]);
            let mut commented = false;
            let mut commented_code = false;
            for m in markers {
                if m.is_empty() {
                    continue;
                }
                if let Some(fi) = memchr::memmem::find(&cl[bl..bh], m) {
                    commented = true;
                    let (stl, sth) = trim_span_b(&cl[bl..bl + fi]);
                    if stl < sth {
                        commented_code = true;
                    }
                }
            }
            if commented {
                if commented_code {
                    code += 1;
                } else {
                    comments += 1;
                }
                continue;
            }
            if !bend.is_empty() && memchr::memmem::find(cl, bend).is_some() {
                if bl >= bh {
                    comments += 1;
                } else {
                    code += 1;
                }
                continue;
            }
            in_block = true;
            depth = 1;
            if bl >= bh || starts_with_any_b(raw, tls, markers) {
                comments += 1;
            } else {
                code += 1;
            }
            continue;
        }
        code += 1;
    }
    (lines, code, comments, blanks)
}

fn count_occ(hay: &[u8], needle: &[u8]) -> i64 {
    if needle.is_empty() {
        return 0;
    }
    memchr::memmem::find_iter(hay, needle).count() as i64
}

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
    match arg(args, 0, "bytes.to_ints")? {
        Value::Bytes(b) => Ok(Value::Array(Box::new(
            b.as_slice().iter().map(|x| Value::Int(*x as i64)).collect(),
        ))),
        other => Err(EvalError::new(
            format!("`bytes.to_ints` expects bytes, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn str_find_in(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.find_in")?;
    let sub = expect_str(args, 1, "std.str.find_in")?;
    let start = expect_int(args, 2, "std.str.find_in")?;
    let end = expect_int(args, 3, "std.str.find_in")?;
    Ok(Value::Int(find_in(
        s.as_bytes(),
        sub.as_bytes(),
        start,
        end,
    )))
}

pub(crate) fn str_rfind_in(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.rfind_in")?;
    let sub = expect_str(args, 1, "std.str.rfind_in")?;
    let start = expect_int(args, 2, "std.str.rfind_in")?;
    let end = expect_int(args, 3, "std.str.rfind_in")?;
    Ok(Value::Int(rfind_in(
        s.as_bytes(),
        sub.as_bytes(),
        start,
        end,
    )))
}

pub(crate) fn str_count_in(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.count_in")?;
    let sub = expect_str(args, 1, "std.str.count_in")?;
    let start = expect_int(args, 2, "std.str.count_in")?;
    let end = expect_int(args, 3, "std.str.count_in")?;
    Ok(Value::Int(count_in(
        s.as_bytes(),
        sub.as_bytes(),
        start,
        end,
    )))
}

// Bounded byte-window scans: the scan never reads past `end`, so per-line
// use stays O(line) and whole-file loops stay O(n). Empty `sub` returns
// the clamped start (find/rfind) or 0 (count).
fn clamp_span(len: i64, start: i64, end: i64) -> (usize, usize) {
    let s = start.clamp(0, len);
    let mut e = end.clamp(0, len);
    if e < s {
        e = s;
    }
    (s as usize, e as usize)
}

fn find_in(hay: &[u8], needle: &[u8], start: i64, end: i64) -> i64 {
    let (s, e) = clamp_span(hay.len() as i64, start, end);
    if needle.is_empty() {
        return s as i64;
    }
    match memchr::memmem::find(&hay[s..e], needle) {
        Some(rel) => s as i64 + rel as i64,
        None => -1,
    }
}

fn rfind_in(hay: &[u8], needle: &[u8], start: i64, end: i64) -> i64 {
    let (s, e) = clamp_span(hay.len() as i64, start, end);
    if needle.is_empty() {
        return e as i64;
    }
    match memchr::memmem::rfind(&hay[s..e], needle) {
        Some(rel) => s as i64 + rel as i64,
        None => -1,
    }
}

fn count_in(hay: &[u8], needle: &[u8], start: i64, end: i64) -> i64 {
    let (mut s, e) = clamp_span(hay.len() as i64, start, end);
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    while s + needle.len() <= e {
        match memchr::memmem::find(&hay[s..e], needle) {
            Some(rel) => {
                n += 1;
                s += rel + needle.len();
            }
            None => break,
        }
    }
    n
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
    // No boundary checks: a valid pattern's first byte (ASCII or lead)
    // can never equal a continuation byte, so mid-char starts cannot
    // match. Byte windows panic on nothing, on any input.
    &bytes[base..base + sub.len()] == sub.as_bytes()
}

// True when `sub` ends at byte offset `pos` (exclusive end).
fn ends_at(s: &str, sub: &str, pos: i64) -> bool {
    if sub.is_empty() {
        return false;
    }
    let bytes = s.as_bytes();
    if pos < 0 {
        return false;
    }
    let end = pos as usize;
    let sub_b = sub.as_bytes();
    if end > bytes.len() || end < sub_b.len() {
        return false;
    }
    let base = end - sub_b.len();
    &bytes[base..end] == sub_b
}

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
    if end - s >= 2 && b[end - 2] == 0xC2 && (c == 0x85 || c == 0xA0) {
        return 2;
    }
    if end - s >= 3 && b[end - 3] == 0xE1 && b[end - 2] == 0x9A && c == 0x80 {
        return 3;
    }
    if end - s >= 3
        && b[end - 3] == 0xE2
        && b[end - 2] == 0x80
        && ((0x80..=0x8A).contains(&c) || c == 0xA8 || c == 0xA9 || c == 0xAF)
    {
        return 3;
    }
    if end - s >= 3 && b[end - 3] == 0xE2 && b[end - 2] == 0x81 && c == 0x9F {
        return 3;
    }
    if end - s >= 3 && b[end - 3] == 0xE3 && b[end - 2] == 0x80 && c == 0x80 {
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
