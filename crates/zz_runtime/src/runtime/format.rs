//! Formatting and pattern-matching helpers extracted from the interpreter.

use crate::value::Value;
use zz_frontend::ast::Lit;

/// Apply a format spec to a value for string interpolation.
///
/// Display semantics: `Option` layers auto-unwrap (`.some(v)` → `v`,
/// `.none` → `none`) before the spec applies, so interpolation never
/// exposes the raw `.some(...)` / `.none` structure. Debug specs
/// (`?`, `debug`) preserve the explicit wrapper form for `dbg`-style
/// output.
///
/// Supported specs:
/// - `.Nf` — float with N decimal places (e.g. `.2f` → `3.14`)
/// - `x` / `X` — hex integer (lowercase / uppercase)
/// - `o` — octal integer
/// - `b` — binary integer
/// - `d` — decimal integer (default for ints)
/// - `e` / `E` — scientific notation
/// - `s` — string (default, no-op)
/// - `?` / `debug` — explicit debug formatting (keeps `.some`/`.none`)
pub(crate) fn format_value_with_spec(v: &Value, spec: &str) -> String {
    let spec = spec.trim();
    // Explicit debug formatting preserves wrappers.
    if spec == "?" || spec == "debug" {
        return v.to_string();
    }
    // Auto-unwrap Option layers for display; `.none` renders as `none`.
    let mut cur = v;
    loop {
        match cur {
            Value::Option(Some(inner)) => cur = inner,
            Value::Option(None) => return "none".to_string(),
            _ => break,
        }
    }
    let v = cur;
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
        // Display (unwrapped) for all other values so nested Options
        // inside arrays/dicts/objects never leak `.some(...)` either.
        other => other.to_display_string(),
    }
}

/// Check whether a runtime value matches a literal pattern.
pub(crate) fn value_matches_lit(value: &Value, lit: &Lit) -> bool {
    match (value, lit) {
        (Value::Int(a), Lit::Int(b)) => a == b,
        (Value::Float(a), Lit::Float(b)) => a == b,
        (Value::Str(a), Lit::Str(b)) => &**a == b,
        (Value::Bool(a), Lit::Bool(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some_int(n: i64) -> Value {
        Value::Option(Some(Box::new(Value::Int(n))))
    }

    #[test]
    fn interpolation_unwraps_option_some() {
        assert_eq!(format_value_with_spec(&some_int(42), ""), "42");
        assert_eq!(format_value_with_spec(&some_int(255), "x"), "ff");
        assert_eq!(
            format_value_with_spec(&Value::Option(Some(Box::new(Value::Float(1.23456)))), ".2f"),
            "1.23"
        );
    }

    #[test]
    fn interpolation_renders_none_without_wrapper() {
        assert_eq!(format_value_with_spec(&Value::Option(None), ""), "none");
        assert_eq!(format_value_with_spec(&Value::Option(None), "x"), "none");
    }

    #[test]
    fn debug_spec_preserves_wrappers() {
        assert_eq!(format_value_with_spec(&some_int(42), "?"), ".some(42)");
        assert_eq!(format_value_with_spec(&some_int(42), "debug"), ".some(42)");
        assert_eq!(format_value_with_spec(&Value::Option(None), "?"), ".none");
    }

    #[test]
    fn nested_options_unwrap_in_containers() {
        let arr = Value::Array(Box::new(vec![some_int(1), Value::Option(None)]));
        assert_eq!(format_value_with_spec(&arr, ""), "[1, none]");
    }
}
