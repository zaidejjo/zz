use crate::natives::arg;
use zz_runtime::{EvalError, Interp, Span, Value};

// ── Helpers ─────────────────────────────────────────────────────────────────

fn expect_float(args: &mut Vec<Value>, i: usize, name: &str) -> Result<f64, EvalError> {
    match arg(args, i, name)? {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        other => Err(EvalError::new(
            format!("`{name}` expects a number, found `{other}`"),
            Span::new(0, 0),
        )),
    }
}

// ── Utilities & Rounding ────────────────────────────────────────────────────

// PNSA: math.abs moved to pure-Zz (scalar.zz, T: Num + Ord).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// PNSA: math.floor/ceil/trunc/round moved to pure-Zz (rounding.zz).
// PNSA: math.abs/signum moved to pure-Zz (scalar.zz, T: Num + Ord).
// Rust bodies removed; symbols resolved from libzz_stdlib.a in AOT.

// PNSA: math.floor/ceil/trunc/round moved to pure-Zz (rounding.zz).
// Rust bodies removed; symbols resolved from libzz_stdlib.a in AOT.

// ── PNSA: clamp moved to pure-ZZ (generic <T: Ord> -> T) ────────────────────
// math.clamp is now PureZz in scalar.zz — handles int, float, str.
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// PNSA: math.signum moved to pure-Zz (scalar.zz, T: Num + Ord).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// PNSA: math.hypot moved to pure-Zz (log.zz).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

pub(crate) fn math_is_nan(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for is_nan", span))?;
    match v {
        Value::Float(f) => Ok(Value::Bool(f.is_nan())),
        Value::Int(_) => Ok(Value::Bool(false)),
        other => Err(EvalError::new(
            format!("is_nan expects a number, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_is_inf(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for is_inf", span))?;
    match v {
        Value::Float(f) => Ok(Value::Bool(f.is_infinite())),
        Value::Int(_) => Ok(Value::Bool(false)),
        other => Err(EvalError::new(
            format!("is_inf expects a number, found `{}`", other.type_name()),
            span,
        )),
    }
}

// ── Number Theory & Integer Math ────────────────────────────────────────────

// PNSA: math.root moved to pure-Zz (log.zz).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// ── PNSA: isqrt moved to pure-Zz (int.zz) ────────────────────────────────────
// math.isqrt uses Newton's method in ZZ (int.zz).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// ── PNSA: factorial/gcd/lcm moved to pure-Zz (int.zz) ────────────────────────
// Rust bodies removed; symbols resolved from libzz_stdlib.a in AOT.
// VM uses Rust bodies above (still present during transition).

// ── Trigonometry ────────────────────────────────────────────────────────────

pub(crate) fn math_sin(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "sin")?;
    Ok(Value::Float(x.sin()))
}

pub(crate) fn math_cos(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "cos")?;
    Ok(Value::Float(x.cos()))
}

pub(crate) fn math_tan(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "tan")?;
    Ok(Value::Float(x.tan()))
}

pub(crate) fn math_asin(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "asin")?;
    Ok(Value::Float(x.asin()))
}

pub(crate) fn math_acos(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "acos")?;
    Ok(Value::Float(x.acos()))
}

pub(crate) fn math_atan(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "atan")?;
    Ok(Value::Float(x.atan()))
}

pub(crate) fn math_sin_deg(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "sin_deg")?;
    Ok(Value::Float(x.to_radians().sin()))
}

pub(crate) fn math_cos_deg(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "cos_deg")?;
    Ok(Value::Float(x.to_radians().cos()))
}

pub(crate) fn math_tan_deg(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "tan_deg")?;
    Ok(Value::Float(x.to_radians().tan()))
}

pub(crate) fn math_to_radians(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let deg = expect_float(args, 0, "to_radians")?;
    Ok(Value::Float(deg.to_radians()))
}

pub(crate) fn math_to_degrees(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let rad = expect_float(args, 0, "to_degrees")?;
    Ok(Value::Float(rad.to_degrees()))
}

// ── Logarithms & Exponents ──────────────────────────────────────────────────

// PNSA: math.log moved to pure-Zz (log.zz, Taylor series).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// PNSA: math.log10 moved to pure-Zz (log.zz).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// PNSA: math.exp moved to pure-Zz (log.zz, Taylor series).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// ── Square root (existing, kept for compat) ─────────────────────────────────

// PNSA: math.sqrt moved to pure-Zz (log.zz, Newton's method).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// PNSA: math.pow moved to pure-Zz (log.zz, exp(y*log(x))).
// Rust body removed; symbol resolved from libzz_stdlib.a in AOT.

// ── PNSA: math.random/rand_range moved to pure-Zz (log.zz).
// Rust bodies removed; symbol resolved from libzz_stdlib.a in AOT.
// Note: ZZ has no mutable global state, so random() returns 0.5.
// A future ZZ feature (mutable globals) could implement a proper LCG.
