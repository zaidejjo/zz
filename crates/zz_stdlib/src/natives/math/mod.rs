use crate::natives::{arg, expect_array};
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

// ── Constants (zero-arg functions) ──────────────────────────────────────────

pub(crate) fn math_pi(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Float(std::f64::consts::PI))
}

pub(crate) fn math_e(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Float(std::f64::consts::E))
}

pub(crate) fn math_tau(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Float(std::f64::consts::TAU))
}

pub(crate) fn math_inf(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Float(f64::INFINITY))
}

pub(crate) fn math_nan(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(Value::Float(f64::NAN))
}

// ── Utilities & Rounding ────────────────────────────────────────────────────

pub(crate) fn math_abs(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for abs", span))?;
    match v {
        Value::Int(i) => Ok(Value::Int(i.abs())),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        other => Err(EvalError::new(
            format!(
                "abs expects `int` or `float`, found `{}`",
                other.type_name()
            ),
            span,
        )),
    }
}

pub(crate) fn math_round(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for round", span))?;
    match v {
        Value::Int(i) => Ok(Value::Int(i)),
        Value::Float(f) => Ok(Value::Float(f.round())),
        other => Err(EvalError::new(
            format!("round expects a number, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_floor(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for floor", span))?;
    match v {
        Value::Float(f) => Ok(Value::Int(f.floor() as i64)),
        Value::Int(i) => Ok(Value::Int(i)),
        other => Err(EvalError::new(
            format!("floor expects `float`, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_ceil(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for ceil", span))?;
    match v {
        Value::Float(f) => Ok(Value::Int(f.ceil() as i64)),
        Value::Int(i) => Ok(Value::Int(i)),
        other => Err(EvalError::new(
            format!("ceil expects `float`, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_trunc(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for trunc", span))?;
    match v {
        Value::Int(i) => Ok(Value::Int(i)),
        Value::Float(f) => Ok(Value::Float(f.trunc())),
        other => Err(EvalError::new(
            format!("trunc expects a number, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_clamp(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let val = expect_float(args, 0, "clamp")?;
    let min = expect_float(args, 1, "clamp")?;
    let max = expect_float(args, 2, "clamp")?;
    Ok(Value::Float(val.clamp(min, max)))
}

pub(crate) fn math_signum(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument for signum", span))?;
    match v {
        Value::Int(i) => Ok(Value::Int(i.signum())),
        Value::Float(f) => Ok(Value::Float(f.signum())),
        other => Err(EvalError::new(
            format!("signum expects a number, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_hypot(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
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
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "root")?;
    let n = expect_float(args, 1, "root")?;
    Ok(Value::Float(x.powf(1.0 / n)))
}

pub(crate) fn math_isqrt(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let n = match args.first() {
        Some(Value::Int(i)) => *i,
        Some(other) => {
            return Err(EvalError::new(
                format!("isqrt expects an integer, found `{}`", other.type_name()),
                span,
            ))
        }
        None => return Err(EvalError::new("missing argument for isqrt", span)),
    };
    if n < 0 {
        return Ok(Value::Result(Err(Box::new(Value::Str(
            "isqrt: negative input".to_string(),
        )))));
    }
    // Newton's method for integer square root
    if n == 0 {
        return Ok(Value::Result(Ok(Box::new(Value::Int(0)))));
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    Ok(Value::Result(Ok(Box::new(Value::Int(x)))))
}

pub(crate) fn math_factorial(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let n = match args.first() {
        Some(Value::Int(i)) => *i,
        Some(other) => {
            return Err(EvalError::new(
                format!(
                    "factorial expects an integer, found `{}`",
                    other.type_name()
                ),
                span,
            ))
        }
        None => return Err(EvalError::new("missing argument for factorial", span)),
    };
    if n < 0 {
        return Ok(Value::Result(Err(Box::new(Value::Str(
            "factorial: negative input".to_string(),
        )))));
    }
    if n > 20 {
        return Ok(Value::Result(Err(Box::new(Value::Str(
            "factorial: input too large (max 20)".to_string(),
        )))));
    }
    let mut result: i64 = 1;
    for i in 2..=n {
        result = result.saturating_mul(i);
    }
    Ok(Value::Result(Ok(Box::new(Value::Int(result)))))
}

pub(crate) fn math_gcd(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let a = expect_float(args, 0, "gcd")? as i64;
    let b = expect_float(args, 1, "gcd")? as i64;
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    Ok(Value::Int(a))
}

pub(crate) fn math_lcm(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let a = expect_float(args, 0, "lcm")? as i64;
    let b = expect_float(args, 1, "lcm")? as i64;
    if a == 0 || b == 0 {
        return Ok(Value::Int(0));
    }
    let (a, b) = (a.abs(), b.abs());
    Ok(Value::Int(a / gcd(a, b) * b))
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

// ── Trigonometry ────────────────────────────────────────────────────────────

pub(crate) fn math_sin(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "sin")?;
    Ok(Value::Float(x.sin()))
}

pub(crate) fn math_cos(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "cos")?;
    Ok(Value::Float(x.cos()))
}

pub(crate) fn math_tan(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "tan")?;
    Ok(Value::Float(x.tan()))
}

pub(crate) fn math_asin(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "asin")?;
    Ok(Value::Float(x.asin()))
}

pub(crate) fn math_acos(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "acos")?;
    Ok(Value::Float(x.acos()))
}

pub(crate) fn math_atan(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "atan")?;
    Ok(Value::Float(x.atan()))
}

pub(crate) fn math_sin_deg(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "sin_deg")?;
    Ok(Value::Float(x.to_radians().sin()))
}

pub(crate) fn math_cos_deg(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "cos_deg")?;
    Ok(Value::Float(x.to_radians().cos()))
}

pub(crate) fn math_tan_deg(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "tan_deg")?;
    Ok(Value::Float(x.to_radians().tan()))
}

pub(crate) fn math_to_radians(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let deg = expect_float(args, 0, "to_radians")?;
    Ok(Value::Float(deg.to_radians()))
}

pub(crate) fn math_to_degrees(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let rad = expect_float(args, 0, "to_degrees")?;
    Ok(Value::Float(rad.to_degrees()))
}

// ── Logarithms & Exponents ──────────────────────────────────────────────────

pub(crate) fn math_log(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "log")?;
    Ok(Value::Float(x.ln()))
}

pub(crate) fn math_log10(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let x = expect_float(args, 0, "log10")?;
    Ok(Value::Float(x.log10()))
}

pub(crate) fn math_exp(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
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

pub(crate) fn math_random(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut state = (nanos as u64) | 1;
    state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let unit = (state >> 33) as f64 / (1u64 << 31) as f64;
    Ok(Value::Float(unit))
}

// ── Linear Algebra & Vectors ────────────────────────────────────────────────

fn as_float_array(v: &Value, name: &str, span: Span) -> Result<Vec<f64>, EvalError> {
    match v {
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                match to_float(item) {
                    Some(f) => out.push(f),
                    None => {
                        return Err(EvalError::new(
                            format!(
                                "`{name}`: element {i} is not a number, found `{}`",
                                item.type_name()
                            ),
                            span,
                        ))
                    }
                }
            }
            Ok(out)
        }
        other => Err(EvalError::new(
            format!("`{name}` expects an array, found `{}`", other.type_name()),
            span,
        )),
    }
}

pub(crate) fn math_dot_product(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v1 = as_float_array(
        args.get(0)
            .ok_or_else(|| EvalError::new("missing argument `v1` for dot_product", span))?,
        "dot_product",
        span,
    )?;
    let v2 = as_float_array(
        args.get(1)
            .ok_or_else(|| EvalError::new("missing argument `v2` for dot_product", span))?,
        "dot_product",
        span,
    )?;
    if v1.len() != v2.len() {
        return Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "dot_product: length mismatch ({} vs {})",
            v1.len(),
            v2.len()
        ))))));
    }
    let sum: f64 = v1.iter().zip(v2.iter()).map(|(a, b)| a * b).sum();
    Ok(Value::Result(Ok(Box::new(Value::Float(sum)))))
}

pub(crate) fn math_magnitude(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let v = as_float_array(
        args.first()
            .ok_or_else(|| EvalError::new("missing argument for magnitude", span))?,
        "magnitude",
        span,
    )?;
    let sum: f64 = v.iter().map(|x| x * x).sum();
    Ok(Value::Float(sum.sqrt()))
}

pub(crate) fn math_matrix_mul(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let m1_val = args
        .get(0)
        .ok_or_else(|| EvalError::new("missing argument `m1` for matrix_mul", span))?;
    let m2_val = args
        .get(1)
        .ok_or_else(|| EvalError::new("missing argument `m2` for matrix_mul", span))?;

    let to_matrix = |v: &Value| -> Result<Vec<Vec<f64>>, EvalError> {
        match v {
            Value::Array(rows) => {
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    out.push(as_float_array(row, "matrix_mul", span)?);
                }
                Ok(out)
            }
            other => Err(EvalError::new(
                format!(
                    "matrix_mul expects a matrix (array of arrays), found `{}`",
                    other.type_name()
                ),
                span,
            )),
        }
    };

    let m1 = to_matrix(m1_val)?;
    let m2 = to_matrix(m2_val)?;

    if m1.is_empty() || m2.is_empty() {
        return Ok(Value::Result(Err(Box::new(Value::Str(
            "matrix_mul: empty matrix".to_string(),
        )))));
    }
    let cols1 = m1[0].len();
    let rows2 = m2.len();
    if cols1 != rows2 {
        return Ok(Value::Result(Err(Box::new(Value::Str(format!(
            "matrix_mul: dimension mismatch ({}x{}) * ({}x{})",
            m1.len(),
            cols1,
            rows2,
            m2[0].len()
        ))))));
    }
    let cols2 = m2[0].len();
    let mut result = vec![vec![0.0f64; cols2]; m1.len()];
    for i in 0..m1.len() {
        for j in 0..cols2 {
            let mut sum = 0.0;
            for k in 0..cols1 {
                sum += m1[i][k] * m2[k][j];
            }
            result[i][j] = sum;
        }
    }
    // Convert to Value::Array of Value::Array of Value::Float
    let out: Vec<Value> = result
        .into_iter()
        .map(|row| Value::Array(row.into_iter().map(Value::Float).collect()))
        .collect();
    Ok(Value::Result(Ok(Box::new(Value::Array(out)))))
}

// ── Basic Statistics & Random ───────────────────────────────────────────────

pub(crate) fn math_mean(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let vals = as_float_array(
        args.first()
            .ok_or_else(|| EvalError::new("missing argument for mean", span))?,
        "mean",
        span,
    )?;
    if vals.is_empty() {
        return Ok(Value::Result(Err(Box::new(Value::Str(
            "mean: empty list".to_string(),
        )))));
    }
    let sum: f64 = vals.iter().sum();
    Ok(Value::Result(Ok(Box::new(Value::Float(
        sum / vals.len() as f64,
    )))))
}

pub(crate) fn math_median(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vals = as_float_array(
        args.first()
            .ok_or_else(|| EvalError::new("missing argument for median", span))?,
        "median",
        span,
    )?;
    if vals.is_empty() {
        return Ok(Value::Result(Err(Box::new(Value::Str(
            "median: empty list".to_string(),
        )))));
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = vals.len();
    let median = if n % 2 == 0 {
        (vals[n / 2 - 1] + vals[n / 2]) / 2.0
    } else {
        vals[n / 2]
    };
    Ok(Value::Result(Ok(Box::new(Value::Float(median)))))
}

pub(crate) fn math_rand_range(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let min = expect_float(args, 0, "rand_range")?;
    let max = expect_float(args, 1, "rand_range")?;
    // Reuse the same LCG as math_random
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut state = (nanos as u64) | 1;
    state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let unit = (state >> 33) as f64 / (1u64 << 31) as f64;
    Ok(Value::Float(min + unit * (max - min)))
}
