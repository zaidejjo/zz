//! `std.crypto` core natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::crypto_core` (which also backs the AOT `zz_crypto_*`
//! FFI). All digests cross as lowercase hex strings; `random_bytes`
//! returns hex of `n` CSPRNG bytes (raw bytes cannot be `str` values,
//! which must be valid UTF-8).

use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::{expect_int, expect_str};

pub(crate) fn crypto_sha256(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.crypto.sha256")?;
    Ok(Value::Str(
        zz_native_rt::crypto_core::sha256_hex(s.as_bytes()).into(),
    ))
}

pub(crate) fn crypto_sha512(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.crypto.sha512")?;
    Ok(Value::Str(
        zz_native_rt::crypto_core::sha512_hex(s.as_bytes()).into(),
    ))
}

pub(crate) fn crypto_hmac_sha256(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let key = expect_str(args, 0, "std.crypto.hmac_sha256")?;
    let msg = expect_str(args, 1, "std.crypto.hmac_sha256")?;
    Ok(Value::Str(
        zz_native_rt::crypto_core::hmac_sha256_hex(key.as_bytes(), msg.as_bytes()).into(),
    ))
}

pub(crate) fn crypto_random_bytes(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let n = expect_int(args, 0, "std.crypto.random_bytes")?;
    if n < 0 {
        return Err(EvalError::new(
            "std.crypto.random_bytes: length must be >= 0".to_string(),
            span,
        ));
    }
    match zz_native_rt::crypto_core::random_hex(n as usize) {
        Ok(h) => Ok(Value::Str(h.into())),
        Err(msg) => Err(EvalError::new(msg, span)),
    }
}

pub(crate) fn crypto_ct_eq(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let a = expect_str(args, 0, "std.crypto.ct_eq")?;
    let b = expect_str(args, 1, "std.crypto.ct_eq")?;
    Ok(Value::Bool(zz_native_rt::crypto_core::ct_eq(
        a.as_bytes(),
        b.as_bytes(),
    )))
}
