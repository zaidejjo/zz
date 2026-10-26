//! `assert`, `assert_eq`, `assert_ne`, `assert_approx_eq`, `fail` natives.
//!
//! Structural diffing is rendered into the `EvalError` message so both
//! `zz run` and the `zz test` harness surface a human-readable failure
//! without a separate reporter. P4's reporter re-uses the same helpers
//! for colored terminal/JUnit rendering.

use zz_runtime::{EvalError, Interp, Span, Value};

fn expect_bool(v: &Value, name: &str, span: Span) -> Result<bool, EvalError> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => Err(EvalError::new(
            format!(
                "`{name}` expects `bool`, found `{other}` ({})",
                other.type_name()
            ),
            span,
        )),
    }
}

fn expect_str(v: &Value, name: &str, span: Span) -> Result<String, EvalError> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(EvalError::new(
            format!(
                "`{name}` expects `str`, found `{other}` ({})",
                other.type_name()
            ),
            span,
        )),
    }
}

fn val_to_string(v: &Value) -> String {
    format!("{v}")
}

// ── diff helpers ─────────────────────────────────────────────────────────

/// Entry: produce a structural diff between two values for assertion messages.
fn diff_values(left: &Value, right: &Value) -> String {
    match (left, right) {
        (Value::Str(a), Value::Str(b)) if a.contains('\n') || b.contains('\n') => {
            diff_multiline_str(a, b)
        }
        (Value::Array(a), Value::Array(b)) => diff_arrays(a, b, 0),
        (Value::Bytes(_), Value::Bytes(_)) => diff_scalar(left, right),
        (Value::Object(a), Value::Object(b)) => diff_objects(a, b, 0),
        (Value::Dict(a), Value::Dict(b)) => diff_dicts(a.entries(), b.entries(), 0),
        _ => diff_scalar(left, right),
    }
}

fn diff_scalar(left: &Value, right: &Value) -> String {
    // Scalar fallback: two lines with Left/Right.
    // P4 will color them red/green; plain here.
    format!(
        "- Left:  {}\n+ Right: {}",
        val_to_string(left),
        val_to_string(right)
    )
}

fn diff_multiline_str(a: &str, b: &str) -> String {
    let a_lines: Vec<&str> = a.split('\n').collect();
    let b_lines: Vec<&str> = b.split('\n').collect();
    let ops = myers_diff(&a_lines, &b_lines);
    let mut out = String::new();
    out.push_str("  Diff (line-based):\n");
    for op in ops {
        match op {
            DiffOp::Equal(s) => {
                out.push_str("    ");
                out.push_str(&s);
                out.push('\n');
            }
            DiffOp::Delete(s) => {
                out.push_str("  - ");
                out.push_str(&s);
                out.push('\n');
            }
            DiffOp::Insert(s) => {
                out.push_str("  + ");
                out.push_str(&s);
                out.push('\n');
            }
        }
    }
    // Word-level hint for adjacent delete/insert pairs: we render a second pass
    // that highlights differing words within the changed lines when run under
    // the test reporter (plain here, but structure is Myers + word LCS).
    out.trim_end().to_string()
}

#[derive(Debug, Clone)]
enum DiffOp {
    Equal(String),
    Delete(String),
    Insert(String),
}

/// Myers-style diff via LCS DP (O(n*m), fine for test strings).
fn myers_diff(a: &[&str], b: &[&str]) -> Vec<DiffOp> {
    let n = a.len();
    let m = b.len();
    // dp[i][j] = LCS length of a[i..] / b[j..] suffix? Build bottom-up 2D.
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if a[i] == b[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }
    let mut ops = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(DiffOp::Equal(a[i].to_string()));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(DiffOp::Delete(a[i].to_string()));
            i += 1;
        } else {
            ops.push(DiffOp::Insert(b[j].to_string()));
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffOp::Delete(a[i].to_string()));
        i += 1;
    }
    while j < m {
        ops.push(DiffOp::Insert(b[j].to_string()));
        j += 1;
    }
    ops
}

fn diff_arrays(a: &[Value], b: &[Value], indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let inner_pad = "  ".repeat(indent + 1);
    // Empty vs empty handled by equality before here; this is diff case.
    let mut out = String::new();
    out.push_str(&format!("{}[\n", pad));
    let max = a.len().max(b.len());
    for idx in 0..max {
        let va = a.get(idx);
        let vb = b.get(idx);
        match (va, vb) {
            (Some(la), Some(ra)) if la == ra => {
                out.push_str(&format!("{}  {}: {},\n", inner_pad, idx, val_to_string(la)));
            }
            (Some(la), Some(ra)) => {
                out.push_str(&format!("{}- {}: {},\n", inner_pad, idx, val_to_string(la)));
                out.push_str(&format!("{}+ {}: {},\n", inner_pad, idx, val_to_string(ra)));
            }
            (Some(la), None) => {
                out.push_str(&format!("{}- {}: {},\n", inner_pad, idx, val_to_string(la)));
            }
            (None, Some(ra)) => {
                out.push_str(&format!("{}+ {}: {},\n", inner_pad, idx, val_to_string(ra)));
            }
            (None, None) => {}
        }
    }
    out.push_str(&format!("{}]", pad));
    out
}

fn diff_objects(
    a: &zz_runtime::value::ObjectValue,
    b: &zz_runtime::value::ObjectValue,
    indent: usize,
) -> String {
    let pad = "  ".repeat(indent);
    let inner_pad = "  ".repeat(indent + 1);
    let name = &a.name;
    let mut out = String::new();
    out.push_str(&format!("{}{} {{\n", pad, name));
    let b_map: std::collections::HashMap<&str, &Value> =
        b.fields.iter().map(|(k, v)| (k.as_ref(), v)).collect();
    // Preserve order of `a`, then any extra `b` fields.
    let mut seen = std::collections::HashSet::new();
    for (k, va) in &a.fields {
        seen.insert(k.as_ref());
        if let Some(vb) = b_map.get(k.as_ref()) {
            if va == *vb {
                out.push_str(&format!("{}  {}: {},\n", inner_pad, k, val_to_string(va)));
            } else {
                out.push_str(&format!("{}- {}: {},\n", inner_pad, k, val_to_string(va)));
                out.push_str(&format!("{}+ {}: {},\n", inner_pad, k, val_to_string(vb)));
            }
        } else {
            out.push_str(&format!("{}- {}: {},\n", inner_pad, k, val_to_string(va)));
        }
    }
    for (k, vb) in &b.fields {
        if !seen.contains(k.as_ref()) {
            out.push_str(&format!("{}+ {}: {},\n", inner_pad, k, val_to_string(vb)));
        }
    }
    out.push_str(&format!("{}}}", pad));
    out
}

fn diff_dicts(a: &[(Value, Value)], b: &[(Value, Value)], indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let inner_pad = "  ".repeat(indent + 1);
    let mut out = String::new();
    out.push_str(&format!("{}{{\n", pad));
    // Index by stringified key for comparison (keys are Values; naive).
    // For diff we just compare by position/value equality.
    let max = a.len().max(b.len());
    for i in 0..max {
        match (a.get(i), b.get(i)) {
            (Some((ka, va)), Some((kb, vb))) if ka == kb && va == vb => {
                out.push_str(&format!(
                    "{}  {}: {},\n",
                    inner_pad,
                    val_to_string(ka),
                    val_to_string(va)
                ));
            }
            (Some((ka, va)), Some((kb, vb))) => {
                out.push_str(&format!(
                    "{}- {}: {},\n",
                    inner_pad,
                    val_to_string(ka),
                    val_to_string(va)
                ));
                out.push_str(&format!(
                    "{}+ {}: {},\n",
                    inner_pad,
                    val_to_string(kb),
                    val_to_string(vb)
                ));
            }
            (Some((ka, va)), None) => {
                out.push_str(&format!(
                    "{}- {}: {},\n",
                    inner_pad,
                    val_to_string(ka),
                    val_to_string(va)
                ));
            }
            (None, Some((kb, vb))) => {
                out.push_str(&format!(
                    "{}+ {}: {},\n",
                    inner_pad,
                    val_to_string(kb),
                    val_to_string(vb)
                ));
            }
            _ => {}
        }
    }
    out.push('}');
    // Trim inner pad for dict closing
    let _ = pad;
    out
}

// ── natives ───────────────────────────────────────────────────────────────

pub(crate) fn assert_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::new(
            "`assert` expects at least 1 argument (cond: bool)",
            span,
        ));
    }
    let cond = expect_bool(&args[0], "assert", span)?;
    if cond {
        Ok(Value::Unit)
    } else {
        let msg = if args.len() >= 2 {
            expect_str(&args[1], "assert", span)?
        } else {
            "expected true".to_string()
        };
        let mut out = String::new();
        out.push_str("Assertion failed");
        if !msg.is_empty() {
            out.push_str(": ");
            out.push_str(&msg);
        }
        Err(EvalError::new(out, span))
    }
}

pub(crate) fn assert_eq_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_eq` expects 2 arguments (left, right)",
            span,
        ));
    }
    let left = args[0].clone();
    let right = args[1].clone();
    if left == right {
        Ok(Value::Unit)
    } else {
        let diff = diff_values(&left, &right);
        let msg = format!("Assertion Failed: Expected equality\n{}", diff);
        Err(EvalError::new(msg, span))
    }
}

pub(crate) fn assert_ne_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_ne` expects 2 arguments (left, right)",
            span,
        ));
    }
    let left = args[0].clone();
    let right = args[1].clone();
    if left != right {
        Ok(Value::Unit)
    } else {
        let msg = format!(
            "Assertion Failed: Expected inequality but both were equal\n  Value: {}",
            val_to_string(&left)
        );
        Err(EvalError::new(msg, span))
    }
}

pub(crate) fn assert_approx_eq_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::new(
            "`assert_approx_eq` expects 3 arguments (left: float, right: float, epsilon: float)",
            span,
        ));
    }
    let left = args[0]
        .to_float()
        .ok_or_else(|| EvalError::new("`assert_approx_eq` left must be numeric", span))?;
    let right = args[1]
        .to_float()
        .ok_or_else(|| EvalError::new("`assert_approx_eq` right must be numeric", span))?;
    let eps = args[2]
        .to_float()
        .ok_or_else(|| EvalError::new("`assert_approx_eq` epsilon must be numeric", span))?;
    if eps < 0.0 {
        return Err(EvalError::new(
            "`assert_approx_eq` epsilon must be >= 0",
            span,
        ));
    }
    let diff = (left - right).abs();
    if diff <= eps {
        Ok(Value::Unit)
    } else {
        let msg = format!(
            "Assertion Failed: Expected approx equality within epsilon {}\n- Left:  {}\n+ Right: {}\n  Diff: {} > epsilon",
            eps, left, right, diff
        );
        Err(EvalError::new(msg, span))
    }
}

pub(crate) fn fail_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let msg = if args.is_empty() {
        "explicit fail()".to_string()
    } else {
        expect_str(&args[0], "fail", span)?
    };
    Err(EvalError::new(format!("fail: {msg}"), span))
}

// ── std.test pro assertions (P1) ────────────────────────────────────
// Messages mirror the C twins in `zz_codegen/src/runtime/core.c` exactly
// (same wording, same `error:` prefix via EvalError rendering). Keep them
// in lockstep: changing one requires changing the other + parity fixture.

fn expect_str_arg(args: &[Value], idx: usize, name: &str, span: Span) -> Result<String, EvalError> {
    match args.get(idx) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        Some(other) => Err(EvalError::new(
            format!(
                "`{name}` expects `str` for arg {}, found `{other}` ({})",
                idx,
                other.type_name()
            ),
            span,
        )),
        None => Err(EvalError::new(
            format!("`{name}` expects {} arguments", idx + 1),
            span,
        )),
    }
}

fn expect_int_arg(args: &[Value], idx: usize, name: &str, span: Span) -> Result<i64, EvalError> {
    match args.get(idx) {
        Some(Value::Int(n)) => Ok(*n),
        Some(other) => Err(EvalError::new(
            format!(
                "`{name}` expects `int` for arg {}, found `{other}` ({})",
                idx,
                other.type_name()
            ),
            span,
        )),
        None => Err(EvalError::new(
            format!("`{name}` expects {} arguments", idx + 1),
            span,
        )),
    }
}

fn measurable_len(v: &Value) -> Option<usize> {
    match v {
        Value::Array(a) => Some(a.len()),
        Value::Str(s) => Some(s.chars().count()),
        Value::Dict(pairs) => Some(pairs.len()),
        Value::Bytes(b) => Some(b.len()),
        _ => None,
    }
}

pub(crate) fn assert_contains_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_contains` expects 2 arguments (haystack: str, needle: str)",
            span,
        ));
    }
    let hay = expect_str_arg(args, 0, "assert_contains", span)?;
    let needle = expect_str_arg(args, 1, "assert_contains", span)?;
    if hay.contains(needle.as_str()) {
        Ok(Value::Unit)
    } else {
        Err(EvalError::new(
            format!("Assertion Failed: Expected \"{hay}\" to contain \"{needle}\""),
            span,
        ))
    }
}

pub(crate) fn assert_starts_with_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_starts_with` expects 2 arguments (s: str, prefix: str)",
            span,
        ));
    }
    let s = expect_str_arg(args, 0, "assert_starts_with", span)?;
    let prefix = expect_str_arg(args, 1, "assert_starts_with", span)?;
    if s.starts_with(prefix.as_str()) {
        Ok(Value::Unit)
    } else {
        Err(EvalError::new(
            format!("Assertion Failed: Expected \"{s}\" to start with \"{prefix}\""),
            span,
        ))
    }
}

pub(crate) fn assert_ends_with_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_ends_with` expects 2 arguments (s: str, suffix: str)",
            span,
        ));
    }
    let s = expect_str_arg(args, 0, "assert_ends_with", span)?;
    let suffix = expect_str_arg(args, 1, "assert_ends_with", span)?;
    if s.ends_with(suffix.as_str()) {
        Ok(Value::Unit)
    } else {
        Err(EvalError::new(
            format!("Assertion Failed: Expected \"{s}\" to end with \"{suffix}\""),
            span,
        ))
    }
}

pub(crate) fn assert_len_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_len` expects 2 arguments (val, n: int)",
            span,
        ));
    }
    let n = expect_int_arg(args, 1, "assert_len", span)?;
    if n < 0 {
        return Err(EvalError::new("`assert_len` expects n >= 0", span));
    }
    match measurable_len(&args[0]) {
        Some(got) if got as i64 == n => Ok(Value::Unit),
        Some(got) => Err(EvalError::new(
            format!("Assertion Failed: Expected length {n}, got {got}"),
            span,
        )),
        None => Err(EvalError::new(
            format!(
                "`assert_len` expects array/str/dict/bytes, found {} ({})",
                args[0],
                args[0].type_name()
            ),
            span,
        )),
    }
}

pub(crate) fn assert_gt_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_gt` expects 2 arguments (left, right)",
            span,
        ));
    }
    let l = args[0]
        .to_float()
        .ok_or_else(|| EvalError::new("`assert_gt` expects numeric (left, right)", span))?;
    let r = args[1]
        .to_float()
        .ok_or_else(|| EvalError::new("`assert_gt` expects numeric (left, right)", span))?;
    if l > r {
        Ok(Value::Unit)
    } else {
        Err(EvalError::new(
            format!("Assertion Failed: Expected {l} > {r}"),
            span,
        ))
    }
}

pub(crate) fn assert_lt_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_lt` expects 2 arguments (left, right)",
            span,
        ));
    }
    let l = args[0]
        .to_float()
        .ok_or_else(|| EvalError::new("`assert_lt` expects numeric (left, right)", span))?;
    let r = args[1]
        .to_float()
        .ok_or_else(|| EvalError::new("`assert_lt` expects numeric (left, right)", span))?;
    if l < r {
        Ok(Value::Unit)
    } else {
        Err(EvalError::new(
            format!("Assertion Failed: Expected {l} < {r}"),
            span,
        ))
    }
}

pub(crate) fn assert_is_some_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::new(
            "`assert_is_some` expects 1 argument (option)",
            span,
        ));
    }
    match &args[0] {
        Value::Option(Some(_)) => Ok(Value::Unit),
        Value::Option(None) => Err(EvalError::new(
            "Assertion Failed: Expected Some, got none".to_string(),
            span,
        )),
        other => Err(EvalError::new(
            format!(
                "`assert_is_some` expects `option`, found `{other}` ({})",
                other.type_name()
            ),
            span,
        )),
    }
}

pub(crate) fn assert_is_none_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::new(
            "`assert_is_none` expects 1 argument (option)",
            span,
        ));
    }
    match &args[0] {
        Value::Option(None) => Ok(Value::Unit),
        Value::Option(Some(_)) => Err(EvalError::new(
            format!("Assertion Failed: Expected none, got {}", args[0]),
            span,
        )),
        other => Err(EvalError::new(
            format!(
                "`assert_is_none` expects `option`, found `{other}` ({})",
                other.type_name()
            ),
            span,
        )),
    }
}

pub(crate) fn assert_is_ok_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::new(
            "`assert_is_ok` expects 1 argument (result)",
            span,
        ));
    }
    match &args[0] {
        Value::Result(b) if b.is_ok() => Ok(Value::Unit),
        Value::Result(_) => Err(EvalError::new(
            format!("Assertion Failed: Expected ok, got {}", args[0]),
            span,
        )),
        other => Err(EvalError::new(
            format!(
                "`assert_is_ok` expects `result`, found `{other}` ({})",
                other.type_name()
            ),
            span,
        )),
    }
}

pub(crate) fn assert_is_err_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::new(
            "`assert_is_err` expects 1 argument (result)",
            span,
        ));
    }
    match &args[0] {
        Value::Result(b) if b.is_err() => Ok(Value::Unit),
        Value::Result(_) => Err(EvalError::new(
            format!("Assertion Failed: Expected err, got {}", args[0]),
            span,
        )),
        other => Err(EvalError::new(
            format!(
                "`assert_is_err` expects `result`, found `{other}` ({})",
                other.type_name()
            ),
            span,
        )),
    }
}

// ── snapshot assertion (P3) ──────────────────────────────────────────
// File-backed golden values. Directory resolution (mirrored in C twin):
// `$ZZ_SNAPSHOTS_DIR` when set and non-empty (the `zz test` runner sets it
// per test file), else `./snapshots` under the process CWD.
// `$ZZ_UPDATE_SNAPSHOTS=1` writes instead of comparing.
// Names are single components: empty, `/`, `\`, `..`, NUL rejected so a
// snapshot can never escape its directory. Stored as raw bytes with a
// fixed `.snap` suffix (exact-byte comparison, no normalization).

pub(crate) const SNAPSHOT_ENV_DIR: &str = "ZZ_SNAPSHOTS_DIR";
pub(crate) const SNAPSHOT_ENV_UPDATE: &str = "ZZ_UPDATE_SNAPSHOTS";

fn snapshot_name_valid(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 {
        return false;
    }
    if name.bytes().any(|b| b == 0 || b == b'/' || b == b'\\') {
        return false;
    }
    // Reject `.`, `..`, and any `..` segment so the join stays inside.
    if name == "." || name == ".." {
        return false;
    }
    // Legit names like `v1..v2` contain `..` without being traversal, but
    // rejecting them keeps both engines' validation trivially identical.
    if name.contains("..") {
        return false;
    }
    true
}

fn snapshot_dir() -> std::path::PathBuf {
    match std::env::var(SNAPSHOT_ENV_DIR) {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => std::path::PathBuf::from("snapshots"),
    }
}

fn snapshot_update_mode() -> bool {
    matches!(std::env::var(SNAPSHOT_ENV_UPDATE), Ok(v) if v == "1")
}

pub(crate) fn assert_snapshot_fn(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::new(
            "`assert_snapshot` expects 2 arguments (actual: str, name: str)",
            span,
        ));
    }
    let actual = expect_str_arg(args, 0, "assert_snapshot", span)?;
    let name = expect_str_arg(args, 1, "assert_snapshot", span)?;
    if !snapshot_name_valid(&name) {
        return Err(EvalError::new(
            "`assert_snapshot` expects a snapshot name without path separators (`/`, `\\`, `..`)",
            span,
        ));
    }
    let path = snapshot_dir().join(format!("{name}.snap"));
    if snapshot_update_mode() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(EvalError::new(
                    format!("test.snapshot: cannot create snapshots dir: ({e})"),
                    span,
                ));
            }
        }
        if let Err(e) = std::fs::write(&path, actual.as_bytes()) {
            return Err(EvalError::new(
                format!("test.snapshot: cannot write snapshot \"{name}\": ({e})"),
                span,
            ));
        }
        return Ok(Value::Unit);
    }
    let expected = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => {
            return Err(EvalError::new(
                format!("Snapshot missing: \"{name}\" (run with --update-snapshots to create)"),
                span,
            ));
        }
    };
    if expected == actual.as_bytes() {
        return Ok(Value::Unit);
    }
    let exp_str = String::from_utf8_lossy(&expected);
    let diff = diff_values(
        &Value::Str(exp_str.into_owned().into()),
        &Value::Str(actual.clone().into()),
    );
    Err(EvalError::new(
        format!("Assertion Failed: Snapshot mismatch for \"{name}\"\n{diff}"),
        span,
    ))
}
