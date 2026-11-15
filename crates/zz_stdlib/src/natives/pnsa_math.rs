//! PNSA (Precompiled Native Stdlib Archive) VM native wrappers for math.
//!
//! These wrappers adapt the ZZ `NativeEntry` ABI
//! (`fn(&mut Interp, &mut Vec<Value>, Span) -> Result<Value, EvalError>`)
//! to the precompiled C-ABI functions in `libzz_stdlib.a`
//! (`zz_value zz_math_*(zz_value, int *err)`).
//!
//! Used by the VM so stdlib math calls dispatch to native C code instead of
//! being interpreted. AOT links `libzz_stdlib.a` directly.

use zz_runtime::{EvalError, Interp, Span, Value};

/// ZZ value tags (mirrors `zz_tag` in `core.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZzTag {
    ZZ_UNIT = 0,
    ZZ_INT = 1,
    ZZ_FLOAT = 2,
    ZZ_BOOL = 3,
    ZZ_STR = 4,
    ZZ_ARRAY = 5,
    ZZ_DICT = 6,
    ZZ_RESULT_OK = 8,
    ZZ_RESULT_ERR = 9,
}

/// Payload union for `zz_value`. Only the fields needed for the math pilot
/// are included; pointer fields are represented as `*mut ZzValue` for the
/// variant payload.
#[repr(C)]
union ZzValuePayload {
    i: i64,
    f: f64,
    b: bool,
    payload: *mut ZzValue,
    _unused: [u8; 0],
}

impl ZzValuePayload {
    fn int(i: i64) -> Self {
        ZzValuePayload { i }
    }
}

/// Mirrors the C `struct zz_value`.
#[repr(C)]
struct ZzValue {
    tag: ZzTag,
    u: ZzValuePayload,
}

impl ZzValue {
    fn int(i: i64) -> Self {
        ZzValue {
            tag: ZzTag::ZZ_INT,
            u: ZzValuePayload::int(i),
        }
    }

    fn is_int(&self) -> bool {
        self.tag == ZzTag::ZZ_INT
    }

    fn as_int(&self) -> i64 {
        unsafe { self.u.i }
    }

    /// Extract the inner `zz_value` from a `Result` variant. Caller must
    /// ensure `tag` is `ZZ_RESULT_OK` or `ZZ_RESULT_ERR`.
    fn variant_payload(&self) -> Option<&ZzValue> {
        match self.tag {
            ZzTag::ZZ_RESULT_OK | ZzTag::ZZ_RESULT_ERR => unsafe { self.u.payload.as_ref() },
            _ => None,
        }
    }
}

extern "C" {
    fn zz_math_factorial(n: ZzValue, err: *mut i32) -> ZzValue;
    fn zz_math_gcd(a: ZzValue, b: ZzValue, err: *mut i32) -> ZzValue;
    fn zz_math_lcm(a: ZzValue, b: ZzValue, err: *mut i32) -> ZzValue;
    /// Release a `zz_value` created by `zz_variant_ok`/`zz_variant_err`
    /// (frees the heap-allocated payload).
    fn zz_release_variant(v: *mut ZzValue);
}

// ── Factorial ────────────────────────────────────────────────────────────

pub(crate) fn pnsa_math_factorial(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let n = match args.first() {
        Some(Value::Int(i)) => *i,
        Some(_) => return Err(EvalError::new("factorial expects an integer", span)),
        None => return Err(EvalError::new("missing argument for factorial", span)),
    };

    if n < 0 {
        return Ok(Value::Result(Box::new(Err(Value::Str(
            "factorial: negative input".to_string().into(),
        )))));
    }
    if n > 20 {
        return Ok(Value::Result(Box::new(Err(Value::Str(
            "factorial: input too large (max 20)".to_string().into(),
        )))));
    }

    let mut err: i32 = 0;
    let result = unsafe { zz_math_factorial(ZzValue::int(n), &mut err) };

    if err != 0 {
        let msg = result
            .variant_payload()
            .and_then(|v| match v.tag {
                ZzTag::ZZ_STR => {
                    // zz_str is opaque from Rust; fall back to a generic msg
                    None
                }
                _ => None,
            })
            .unwrap_or_else(|| "factorial error".to_string());
        unsafe { zz_release_variant(&result as *const ZzValue as *mut ZzValue) };
        return Ok(Value::Result(Box::new(Err(Value::Str(msg.into())))));
    }

    let int_val = result
        .variant_payload()
        .and_then(|v| if v.is_int() { Some(v.as_int()) } else { None })
        .unwrap_or(0);
    unsafe { zz_release_variant(&result as *const ZzValue as *mut ZzValue) };

    Ok(Value::Result(Box::new(Ok(Value::Int(int_val)))))
}

// ── GCD ──────────────────────────────────────────────────────────────────

pub(crate) fn pnsa_math_gcd(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let a = match args.first() {
        Some(Value::Int(i)) => *i,
        Some(_) => return Err(EvalError::new("gcd expects an integer", _span)),
        None => return Err(EvalError::new("missing argument for gcd", _span)),
    };
    let b = match args.get(1) {
        Some(Value::Int(i)) => *i,
        Some(_) => return Err(EvalError::new("gcd expects an integer", _span)),
        None => return Err(EvalError::new("missing argument for gcd", _span)),
    };

    let mut err: i32 = 0;
    let result = unsafe { zz_math_gcd(ZzValue::int(a), ZzValue::int(b), &mut err) };

    if err != 0 {
        unsafe { zz_release_variant(&result as *const ZzValue as *mut ZzValue) };
        return Err(EvalError::new("gcd failed", _span));
    }

    let int_val = result
        .variant_payload()
        .and_then(|v| if v.is_int() { Some(v.as_int()) } else { None })
        .unwrap_or(0);
    unsafe { zz_release_variant(&result as *const ZzValue as *mut ZzValue) };

    Ok(Value::Int(int_val))
}

// ── LCM ──────────────────────────────────────────────────────────────────

pub(crate) fn pnsa_math_lcm(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let a = match args.first() {
        Some(Value::Int(i)) => *i,
        Some(_) => return Err(EvalError::new("lcm expects an integer", _span)),
        None => return Err(EvalError::new("missing argument for lcm", _span)),
    };
    let b = match args.get(1) {
        Some(Value::Int(i)) => *i,
        Some(_) => return Err(EvalError::new("lcm expects an integer", _span)),
        None => return Err(EvalError::new("missing argument for lcm", _span)),
    };

    let mut err: i32 = 0;
    let result = unsafe { zz_math_lcm(ZzValue::int(a), ZzValue::int(b), &mut err) };

    if err != 0 {
        unsafe { zz_release_variant(&result as *const ZzValue as *mut ZzValue) };
        return Err(EvalError::new("lcm failed", _span));
    }

    let int_val = result
        .variant_payload()
        .and_then(|v| if v.is_int() { Some(v.as_int()) } else { None })
        .unwrap_or(0);
    unsafe { zz_release_variant(&result as *const ZzValue as *mut ZzValue) };

    Ok(Value::Int(int_val))
}
