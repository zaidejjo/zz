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

// PNSA: math.is_nan / math.is_inf moved to pure-Zz (log.zz).
// is_nan(x) = x != x ; is_inf(x) = x == INF || x == -INF.
// Rust bodies removed; symbols resolved from libzz_stdlib.a in AOT.

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
