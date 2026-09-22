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
        (Value::Dict(a), Value::Dict(b)) => diff_dicts(a, b, 0),
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
    let b_map: std::collections::HashMap<&String, &Value> =
        b.fields.iter().map(|(k, v)| (k, v)).collect();
    // Preserve order of `a`, then any extra `b` fields.
    let mut seen = std::collections::HashSet::new();
    for (k, va) in &a.fields {
        seen.insert(k);
        if let Some(vb) = b_map.get(k) {
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
        if !seen.contains(k) {
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
