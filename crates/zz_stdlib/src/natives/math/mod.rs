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

fn to_float(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
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

pub(crate) fn math_hypot(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "hypot")?;
    let y = expect_float(args, 1, "hypot")?;
    Ok(Value::Float(x.hypot(y)))
}

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

pub(crate) fn math_root(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "root")?;
    let n = expect_float(args, 1, "root")?;
    Ok(Value::Float(x.powf(1.0 / n)))
}

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

pub(crate) fn math_log(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "log")?;
    Ok(Value::Float(x.ln()))
}

pub(crate) fn math_log10(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "log10")?;
    Ok(Value::Float(x.log10()))
}

pub(crate) fn math_exp(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "exp")?;
    Ok(Value::Float(x.exp()))
}

// ── Square root (existing, kept for compat) ─────────────────────────────────

pub(crate) fn math_sqrt(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for sqrt", span))?;
    match v {
        Value::Int(i) => Ok(Value::Float((i as f64).sqrt())),
        Value::Float(f) => Ok(Value::Float(f.sqrt())),
        other => Err(EvalError::new(
            format!(
                "sqrt expects `int` or `float`, found `{}`",
                other.type_name()
            ),
            span,
        )),
    }
}

pub(crate) fn math_pow(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let base = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for pow", span))?;
    let exp = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for pow", span))?;
    let (b, e) = match (to_float(&base), to_float(&exp)) {
        (Some(b), Some(e)) => (b, e),
        _ => {
            return Err(EvalError::new(
                format!(
                    "pow expects `int` or `float` arguments, found `{}` and `{}`",
                    base.type_name(),
                    exp.type_name()
                ),
                span,
            ))
        }
    };
    Ok(Value::Float(b.powf(e)))
}

// Deterministic LCG seed so VM and AOT produce identical random sequences
// (parity requires backend-identical output). Seeding from wall-clock time
// made math_random/rand_range differ between engines every run.
const MATH_RAND_SEED: u64 = 0x9E3779B97F4A7C15;

pub(crate) fn math_random(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let mut state = MATH_RAND_SEED;
    state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let unit = (state >> 33) as f64 / (1u64 << 31) as f64;
    Ok(Value::Float(unit))
}

// ── Linear Algebra & Vectors ────────────────────────────────────────────────
// All linear algebra functions moved to pure-Zz (linalg.zz):
//   dot_product, magnitude, matrix_mul
// Rust bodies and C twins removed; symbols resolved from libzz_stdlib.a in AOT.

// ── Basic Statistics & Random ───────────────────────────────────────────────

// ── PNSA: mean/median moved to pure-Zz (stats.zz) ────────────────────────────
// math.mean/math.median delegate to mean_f/median_f (PureZz).
// Rust bodies removed; symbols resolved from libzz_stdlib.a in AOT.

pub(crate) fn math_rand_range(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let min = expect_float(args, 0, "rand_range")?;
    let max = expect_float(args, 1, "rand_range")?;
    // Reuse the same deterministic LCG as math_random (MATH_RAND_SEED).
    let mut state = MATH_RAND_SEED;
    state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let unit = (state >> 33) as f64 / (1u64 << 31) as f64;
    Ok(Value::Float(min + unit * (max - min)))
}
