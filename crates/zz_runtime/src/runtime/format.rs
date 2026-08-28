//! Formatting and pattern-matching helpers extracted from the interpreter.

use crate::value::Value;
use zz_frontend::ast::Lit;

/// Apply a format spec to a value for string interpolation.
///
/// Supported specs:
/// - `.Nf` — float with N decimal places (e.g. `.2f` → `3.14`)
/// - `x` / `X` — hex integer (lowercase / uppercase)
/// - `o` — octal integer
/// - `b` — binary integer
/// - `d` — decimal integer (default for ints)
/// - `e` / `E` — scientific notation
/// - `s` — string (default, no-op)
pub(crate) fn format_value_with_spec(v: &Value, spec: &str) -> String {
    let spec = spec.trim();
    match v {
        Value::Int(n) => {
            if spec == "x" {
                format!("{n:x}")
            } else if spec == "X" {
                format!("{n:X}")
            } else if spec == "o" {
                format!("{n:o}")
            } else if spec == "b" {
                format!("{n:b}")
            } else {
                // "d", empty, or unrecognized specs all produce default decimal.
                format!("{n}")
            }
        }
        Value::Float(f) => {
            if let Some(precision) = spec.strip_suffix('f') {
                let precision: usize = precision.trim_start_matches('.').parse().unwrap_or(0);
                format!("{f:.precision$}")
            } else if spec == "e" {
                format!("{f:e}")
            } else if spec == "E" {
                format!("{f:E}")
            } else if spec == "x" || spec == "X" {
                // Reinterpret the float bits as integer for hex display.
                format!("{:?}", f)
            } else {
                format!("{f}")
            }
        }
        other => other.to_string(),
    }
}

/// Check whether a runtime value matches a literal pattern.
pub(crate) fn value_matches_lit(value: &Value, lit: &Lit) -> bool {
    match (value, lit) {
        (Value::Int(a), Lit::Int(b)) => a == b,
        (Value::Float(a), Lit::Float(b)) => a == b,
        (Value::Str(a), Lit::Str(b)) => a == b,
        (Value::Bool(a), Lit::Bool(b)) => a == b,
        _ => false,
    }
}
