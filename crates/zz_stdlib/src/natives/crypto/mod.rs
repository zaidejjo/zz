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

/// `crypto.sha256_bytes(bytes) -> str` — lowercase hex SHA-256 over raw
/// bytes (binary tarballs cannot round-trip through UTF-8 strings).
pub(crate) fn crypto_sha256_bytes(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match crate::natives::arg(args, 0, "std.crypto.sha256_bytes")? {
        Value::Bytes(b) => Ok(Value::Str(
            zz_native_rt::crypto_core::sha256_hex(b.as_slice()).into(),
        )),
        other => Err(EvalError::new(
            format!("std.crypto.sha256_bytes: expected bytes, found `{other}`"),
            span,
        )),
    }
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

pub(crate) fn crypto_argon2_hash(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let pw = expect_str(args, 0, "std.crypto.argon2_hash")?;
    match zz_native_rt::crypto_pw::argon2_hash(pw.as_bytes()) {
        Ok(h) => Ok(Value::Str(h.into())),
        Err(msg) => Err(EvalError::new(msg, span)),
    }
}

pub(crate) fn crypto_argon2_verify(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let hash = expect_str(args, 0, "std.crypto.argon2_verify")?;
    let pw = expect_str(args, 1, "std.crypto.argon2_verify")?;
    Ok(Value::Bool(zz_native_rt::crypto_pw::argon2_verify(
        &hash,
        pw.as_bytes(),
    )))
}

pub(crate) fn crypto_bcrypt_hash(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let pw = expect_str(args, 0, "std.crypto.bcrypt_hash")?;
    match zz_native_rt::crypto_pw::bcrypt_hash(pw.as_bytes()) {
        Ok(h) => Ok(Value::Str(h.into())),
        Err(msg) => Err(EvalError::new(msg, span)),
    }
}

pub(crate) fn crypto_bcrypt_verify(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let hash = expect_str(args, 0, "std.crypto.bcrypt_verify")?;
    let pw = expect_str(args, 1, "std.crypto.bcrypt_verify")?;
    Ok(Value::Bool(zz_native_rt::crypto_pw::bcrypt_verify(
        &hash,
        pw.as_bytes(),
    )))
}

fn ok_wrap(v: Value) -> Value {
    Value::Result(Box::new(Ok(v)))
}

fn err_wrap(msg: String) -> Value {
    Value::Result(Box::new(Err(Value::Str(msg.into()))))
}

fn str_pair(a: String, b: String) -> Value {
    Value::Array(Box::new(vec![Value::Str(a.into()), Value::Str(b.into())]))
}

pub(crate) fn crypto_ed25519_keypair(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let (sk, pk) = zz_native_rt::crypto_asym::ed25519_keypair();
    Ok(str_pair(sk, pk))
}

pub(crate) fn crypto_ed25519_sign(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let sk = expect_str(args, 0, "std.crypto.ed25519_sign")?;
    let msg = expect_str(args, 1, "std.crypto.ed25519_sign")?;
    match zz_native_rt::crypto_asym::ed25519_sign(&sk, msg.as_bytes()) {
        Ok(sig) => Ok(ok_wrap(Value::Str(sig.into()))),
        Err(e) => Ok(err_wrap(e)),
    }
}

pub(crate) fn crypto_ed25519_verify(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let pk = expect_str(args, 0, "std.crypto.ed25519_verify")?;
    let msg = expect_str(args, 1, "std.crypto.ed25519_verify")?;
    let sig = expect_str(args, 2, "std.crypto.ed25519_verify")?;
    Ok(Value::Bool(zz_native_rt::crypto_asym::ed25519_verify(
        &pk,
        msg.as_bytes(),
        &sig,
    )))
}

pub(crate) fn crypto_rsa_keypair(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    match zz_native_rt::crypto_asym::rsa_keypair() {
        Ok((sk, pk)) => Ok(ok_wrap(str_pair(sk, pk))),
        Err(e) => Err(EvalError::new(e, span)),
    }
}

pub(crate) fn crypto_rsa_sign(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let sk = expect_str(args, 0, "std.crypto.rsa_sign")?;
    let msg = expect_str(args, 1, "std.crypto.rsa_sign")?;
    match zz_native_rt::crypto_asym::rsa_sign(&sk, msg.as_bytes()) {
        Ok(sig) => Ok(ok_wrap(Value::Str(sig.into()))),
        Err(e) => Ok(err_wrap(e)),
    }
}

pub(crate) fn crypto_rsa_verify(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let pk = expect_str(args, 0, "std.crypto.rsa_verify")?;
    let msg = expect_str(args, 1, "std.crypto.rsa_verify")?;
    let sig = expect_str(args, 2, "std.crypto.rsa_verify")?;
    Ok(Value::Bool(zz_native_rt::crypto_asym::rsa_verify(
        &pk,
        msg.as_bytes(),
        &sig,
    )))
}

pub(crate) fn crypto_jwt_encode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let payload = expect_str(args, 0, "std.crypto.jwt_encode")?;
    let secret = expect_str(args, 1, "std.crypto.jwt_encode")?;
    Ok(Value::Str(
        zz_native_rt::crypto_asym::jwt_encode(&payload, &secret).into(),
    ))
}

pub(crate) fn crypto_jwt_decode(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let token = expect_str(args, 0, "std.crypto.jwt_decode")?;
    let secret = expect_str(args, 1, "std.crypto.jwt_decode")?;
    match zz_native_rt::crypto_asym::jwt_decode(&token, &secret) {
        Ok(p) => Ok(ok_wrap(Value::Str(p.into()))),
        Err(e) => Ok(err_wrap(e)),
    }
}

pub(crate) fn crypto_jwt_encode_ed(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let payload = expect_str(args, 0, "std.crypto.jwt_encode_ed")?;
    let sk = expect_str(args, 1, "std.crypto.jwt_encode_ed")?;
    match zz_native_rt::crypto_asym::jwt_encode_ed(&payload, &sk) {
        Ok(t) => Ok(ok_wrap(Value::Str(t.into()))),
        Err(e) => Ok(err_wrap(e)),
    }
}

pub(crate) fn crypto_jwt_decode_ed(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let token = expect_str(args, 0, "std.crypto.jwt_decode_ed")?;
    let pk = expect_str(args, 1, "std.crypto.jwt_decode_ed")?;
    match zz_native_rt::crypto_asym::jwt_decode_ed(&token, &pk) {
        Ok(p) => Ok(ok_wrap(Value::Str(p.into()))),
        Err(e) => Ok(err_wrap(e)),
    }
}
