//! `std.crypto` asymmetric crypto + JWT for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! through thin `zz_stdlib` adapters, while AOT binaries call the
//! `extern "C"` `zz_crypto_*` functions (same `zz_value` convention).
//!
//! Wire formats (all printable, so VM `str` and AOT `zz_str` agree):
//! - Ed25519 secret/public keys: 64 lowercase hex chars (32 bytes).
//! - Ed25519 signatures: 128 hex chars (64 bytes).
//! - RSA keys: PKCS#8 (private) / SubjectPublicKeyInfo (public) PEM.
//! - RSA signatures: hex of PKCS#1v15+SHA-256.
//! - JWT: standard `header.payload.sig` with base64url (no padding).
//!
//! Verification is total: malformed keys, signatures, tokens, and
//! expired `exp` claims all yield `false`/`.err`, never panics.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::{RsaPrivateKey, RsaPublicKey};

use crate::cabi::{cvalue_str, cvalue_to_string, CValue};
use crate::crypto_core::{ct_eq, hmac_sha256_hex};

fn hex(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(ALPHABET[(b >> 4) as usize] as char);
        out.push(ALPHABET[(b & 0x0f) as usize] as char);
    }
    out
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

// --- base64url (no padding; avoids a dependency for ~20 lines) --------------

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[((n >> 18) & 63) as usize] as char);
        out.push(B64URL[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[(n & 63) as usize] as char);
        }
    }
    out
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    // Unpadded length must not leave a dangling single char.
    if s.len() % 4 == 1 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// --- Ed25519 -----------------------------------------------------------------

/// Fresh `(secret_hex, public_hex)` keypair.
pub fn ed25519_keypair() -> (String, String) {
    let sk = SigningKey::generate(&mut rand_core::OsRng);
    let pk = sk.verifying_key();
    (hex(&sk.to_bytes()), hex(pk.as_bytes()))
}

fn ed_sk(sk_hex: &str) -> Option<SigningKey> {
    let bytes = unhex(sk_hex)?;
    SigningKey::from_bytes(bytes.as_slice().try_into().ok()?).into()
}

fn ed_pk(pk_hex: &str) -> Option<VerifyingKey> {
    let bytes = unhex(pk_hex)?;
    VerifyingKey::from_bytes(bytes.as_slice().try_into().ok()?).ok()
}

/// Sign `msg` with `sk_hex`. `Err` on malformed keys.
pub fn ed25519_sign(sk_hex: &str, msg: &[u8]) -> Result<String, String> {
    let Some(sk) = ed_sk(sk_hex) else {
        return Err("crypto.ed25519_sign: malformed secret key (want 64 hex chars)".to_string());
    };
    Ok(hex(&sk.sign(msg).to_bytes()))
}

/// True on valid signature; false on any malformed input or mismatch.
pub fn ed25519_verify(pk_hex: &str, msg: &[u8], sig_hex: &str) -> bool {
    let (Some(pk), Some(sig_bytes)) = (ed_pk(pk_hex), unhex(sig_hex)) else {
        return false;
    };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(&sig_bytes) else {
        return false;
    };
    use ed25519_dalek::Verifier;
    pk.verify(msg, &sig).is_ok()
}

// --- RSA-2048 (PKCS#1v15 + SHA-256) -------------------------------------------

/// Fresh `(private_pem, public_pem)` keypair (PKCS#8 / SPKI, LF endings).
pub fn rsa_keypair() -> Result<(String, String), String> {
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    let sk =
        RsaPrivateKey::new(&mut rand_core::OsRng, 2048).map_err(|e| format!("RSA keygen: {e}"))?;
    let pk = RsaPublicKey::from(&sk);
    let sk_pem = sk
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| format!("RSA PEM encode: {e}"))?;
    let pk_pem = pk
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| format!("RSA PEM encode: {e}"))?;
    Ok((sk_pem.to_string(), pk_pem.to_string()))
}

fn rsa_sk(pem: &str) -> Option<RsaPrivateKey> {
    use rsa::pkcs8::DecodePrivateKey;
    RsaPrivateKey::from_pkcs8_pem(pem).ok()
}

fn rsa_pk(pem: &str) -> Option<RsaPublicKey> {
    use rsa::pkcs8::DecodePublicKey;
    RsaPublicKey::from_public_key_pem(pem).ok()
}

/// Sign `msg` (prehashed with SHA-256 inside) with `sk_pem`. Hex output.
pub fn rsa_sign(sk_pem: &str, msg: &[u8]) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let Some(sk) = rsa_sk(sk_pem) else {
        return Err("crypto.rsa_sign: malformed private key PEM".to_string());
    };
    let digest = Sha256::digest(msg);
    sk.sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
        .map(|s| hex(&s))
        .map_err(|e| format!("crypto.rsa_sign failed: {e}"))
}

/// True on valid signature; false on any malformed input or mismatch.
pub fn rsa_verify(pk_pem: &str, msg: &[u8], sig_hex: &str) -> bool {
    use sha2::{Digest, Sha256};
    let (Some(pk), Some(sig)) = (rsa_pk(pk_pem), unhex(sig_hex)) else {
        return false;
    };
    let digest = Sha256::digest(msg);
    pk.verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &sig)
        .is_ok()
}

// --- JWT ---------------------------------------------------------------------

fn jwt_split(token: &str) -> Option<(&str, &str, &str)> {
    let mut parts = token.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) => Some((h, p, s)),
        _ => None,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Enforce `exp` (seconds since epoch) when present and numeric.
/// Missing/non-numeric `exp` passes (no expiry claimed).
fn exp_ok(payload_json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return false;
    };
    match v.get("exp").and_then(|e| e.as_u64()) {
        Some(exp) => unix_now() < exp,
        None => true,
    }
}

/// HS256 token for `payload_json` (caller supplies JSON text).
pub fn jwt_encode(payload_json: &str, secret: &str) -> String {
    let header = b64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = b64url_encode(payload_json.as_bytes());
    let signing_input = format!("{header}.{payload}");
    let sig = hmac_sha256_hex(secret.as_bytes(), signing_input.as_bytes());
    let sig_b64 = b64url_encode(&unhex(&sig).expect("hex of HMAC is valid hex"));
    format!("{signing_input}.{sig_b64}")
}

/// Verify HS256 `token`; `Ok(payload_json)` on success.
pub fn jwt_decode(token: &str, secret: &str) -> Result<String, String> {
    let Some((h, p, s)) = jwt_split(token) else {
        return Err("crypto.jwt_decode: want header.payload.signature".to_string());
    };
    let signing_input = format!("{h}.{p}");
    let expect = hmac_sha256_hex(secret.as_bytes(), signing_input.as_bytes());
    let expect_b64 = b64url_encode(&unhex(&expect).expect("hex of HMAC is valid hex"));
    if !ct_eq(expect_b64.as_bytes(), s.as_bytes()) {
        return Err("crypto.jwt_decode: bad signature".to_string());
    }
    let payload_bytes =
        b64url_decode(p).ok_or_else(|| "crypto.jwt_decode: bad payload encoding".to_string())?;
    let payload = String::from_utf8(payload_bytes)
        .map_err(|_| "crypto.jwt_decode: payload is not UTF-8".to_string())?;
    if serde_json::from_str::<serde_json::Value>(&payload).is_err() {
        return Err("crypto.jwt_decode: payload is not JSON".to_string());
    }
    if !exp_ok(&payload) {
        return Err("crypto.jwt_decode: token expired".to_string());
    }
    Ok(payload)
}

/// EdDSA (Ed25519) token for `payload_json`.
pub fn jwt_encode_ed(payload_json: &str, sk_hex: &str) -> Result<String, String> {
    let header = b64url_encode(br#"{"alg":"EdDSA","typ":"JWT"}"#);
    let payload = b64url_encode(payload_json.as_bytes());
    let signing_input = format!("{header}.{payload}");
    let Some(sk) = ed_sk(sk_hex) else {
        return Err("crypto.jwt_encode_ed: malformed secret key".to_string());
    };
    let sig = sk.sign(signing_input.as_bytes());
    Ok(format!(
        "{signing_input}.{}",
        b64url_encode(&sig.to_bytes())
    ))
}

/// Verify EdDSA `token`; `Ok(payload_json)` on success.
pub fn jwt_decode_ed(token: &str, pk_hex: &str) -> Result<String, String> {
    use ed25519_dalek::Verifier;
    let Some((h, p, s)) = jwt_split(token) else {
        return Err("crypto.jwt_decode_ed: want header.payload.signature".to_string());
    };
    let signing_input = format!("{h}.{p}");
    let (Some(pk), Some(sig_bytes)) = (ed_pk(pk_hex), b64url_decode(s)) else {
        return Err("crypto.jwt_decode_ed: bad key or signature encoding".to_string());
    };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(&sig_bytes) else {
        return Err("crypto.jwt_decode_ed: bad signature".to_string());
    };
    pk.verify(signing_input.as_bytes(), &sig)
        .map_err(|_| "crypto.jwt_decode_ed: bad signature".to_string())?;
    let payload_bytes =
        b64url_decode(p).ok_or_else(|| "crypto.jwt_decode_ed: bad payload encoding".to_string())?;
    let payload = String::from_utf8(payload_bytes)
        .map_err(|_| "crypto.jwt_decode_ed: payload is not UTF-8".to_string())?;
    if serde_json::from_str::<serde_json::Value>(&payload).is_err() {
        return Err("crypto.jwt_decode_ed: payload is not JSON".to_string());
    }
    if !exp_ok(&payload) {
        return Err("crypto.jwt_decode_ed: token expired".to_string());
    }
    Ok(payload)
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

fn ok_wrap(s: &str) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_ok(cvalue_str(s.as_bytes())) }
}

fn err_wrap(s: &str) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe { crate::cabi::zz_variant_err(cvalue_str(s.as_bytes())) }
}

/// `crypto.ed25519_keypair() -> [str]` (`[secret_hex, public_hex]`).
#[no_mangle]
pub extern "C" fn zz_crypto_ed25519_keypair(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe {
        let arr = crate::cabi::zz_array_new();
        let (sk, pk) = ed25519_keypair();
        crate::cabi::array_push_str(arr, &sk);
        crate::cabi::array_push_str(arr, &pk);
        arr
    }
}

/// `crypto.ed25519_sign(sk: str, msg: str) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_crypto_ed25519_sign(
    sk: CValue,
    msg: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (cvalue_to_string(sk), cvalue_to_string(msg)) {
        (Some(k), Some(m)) => match ed25519_sign(&k, m.as_bytes()) {
            Ok(sig) => ok_wrap(&sig),
            Err(e) => err_wrap(&e),
        },
        _ => {
            set_err(err);
            err_wrap("crypto.ed25519_sign: want (str, str)")
        }
    }
}

/// `crypto.ed25519_verify(pk: str, msg: str, sig: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_crypto_ed25519_verify(
    pk: CValue,
    msg: CValue,
    sig: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (
        cvalue_to_string(pk),
        cvalue_to_string(msg),
        cvalue_to_string(sig),
    ) {
        (Some(k), Some(m), Some(s)) => CValue::boolean(ed25519_verify(&k, m.as_bytes(), &s)),
        _ => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `crypto.rsa_keypair() -> Result<[str], str>` (`[private_pem, public_pem]`).
#[no_mangle]
pub extern "C" fn zz_crypto_rsa_keypair(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    match rsa_keypair() {
        // SAFETY: constructors are provided by the linked AOT program.
        Ok((sk, pk)) => unsafe {
            let arr = crate::cabi::zz_array_new();
            crate::cabi::array_push_str(arr, &sk);
            crate::cabi::array_push_str(arr, &pk);
            crate::cabi::zz_variant_ok(arr)
        },
        Err(e) => err_wrap(&e),
    }
}

/// `crypto.rsa_sign(sk: str, msg: str) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_crypto_rsa_sign(sk: CValue, msg: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (cvalue_to_string(sk), cvalue_to_string(msg)) {
        (Some(k), Some(m)) => match rsa_sign(&k, m.as_bytes()) {
            Ok(sig) => ok_wrap(&sig),
            Err(e) => err_wrap(&e),
        },
        _ => {
            set_err(err);
            err_wrap("crypto.rsa_sign: want (str, str)")
        }
    }
}

/// `crypto.rsa_verify(pk: str, msg: str, sig: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_crypto_rsa_verify(
    pk: CValue,
    msg: CValue,
    sig: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (
        cvalue_to_string(pk),
        cvalue_to_string(msg),
        cvalue_to_string(sig),
    ) {
        (Some(k), Some(m), Some(s)) => CValue::boolean(rsa_verify(&k, m.as_bytes(), &s)),
        _ => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `crypto.jwt_encode(payload: str, secret: str) -> str`.
#[no_mangle]
pub extern "C" fn zz_crypto_jwt_encode(
    payload: CValue,
    secret: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (cvalue_to_string(payload), cvalue_to_string(secret)) {
        // SAFETY: constructors are provided by the linked AOT program.
        (Some(p), Some(s)) => cvalue_str(jwt_encode(&p, &s).as_bytes()),
        _ => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `crypto.jwt_decode(token: str, secret: str) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_crypto_jwt_decode(
    token: CValue,
    secret: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (cvalue_to_string(token), cvalue_to_string(secret)) {
        (Some(t), Some(s)) => match jwt_decode(&t, &s) {
            Ok(p) => ok_wrap(&p),
            Err(e) => err_wrap(&e),
        },
        _ => {
            set_err(err);
            err_wrap("crypto.jwt_decode: want (str, str)")
        }
    }
}

/// `crypto.jwt_encode_ed(payload: str, sk: str) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_crypto_jwt_encode_ed(
    payload: CValue,
    sk: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (cvalue_to_string(payload), cvalue_to_string(sk)) {
        (Some(p), Some(k)) => match jwt_encode_ed(&p, &k) {
            Ok(t) => ok_wrap(&t),
            Err(e) => err_wrap(&e),
        },
        _ => {
            set_err(err);
            err_wrap("crypto.jwt_encode_ed: want (str, str)")
        }
    }
}

/// `crypto.jwt_decode_ed(token: str, pk: str) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_crypto_jwt_decode_ed(
    token: CValue,
    pk: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (cvalue_to_string(token), cvalue_to_string(pk)) {
        (Some(t), Some(k)) => match jwt_decode_ed(&t, &k) {
            Ok(p) => ok_wrap(&p),
            Err(e) => err_wrap(&e),
        },
        _ => {
            set_err(err);
            err_wrap("crypto.jwt_decode_ed: want (str, str)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64url_roundtrip_vectors() {
        assert_eq!(b64url_encode(b""), "");
        assert_eq!(b64url_encode(b"f"), "Zg");
        assert_eq!(b64url_encode(b"fo"), "Zm8");
        assert_eq!(b64url_encode(b"foo"), "Zm9v");
        assert_eq!(b64url_encode(b"foob"), "Zm9vYg");
        for data in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"hello world!?~",
        ] {
            assert_eq!(b64url_decode(&b64url_encode(data)).unwrap(), data);
        }
        assert!(b64url_decode("a").is_none());
        assert!(b64url_decode("****").is_none());
    }

    #[test]
    fn ed25519_roundtrip() {
        let (sk, pk) = ed25519_keypair();
        assert_eq!(sk.len(), 64);
        assert_eq!(pk.len(), 64);
        let sig = ed25519_sign(&sk, b"message").expect("sign");
        assert!(ed25519_verify(&pk, b"message", &sig));
        assert!(!ed25519_verify(&pk, b"tampered", &sig));
        assert!(!ed25519_verify(&pk, b"message", &"00".repeat(64)));
        assert!(!ed25519_verify("zz", b"message", &sig));
        assert!(ed25519_sign("zz", b"message").is_err());
    }

    #[test]
    fn rsa_roundtrip() {
        let (sk, pk) = rsa_keypair().expect("keygen");
        assert!(sk.contains("PRIVATE KEY"));
        assert!(pk.contains("PUBLIC KEY"));
        let sig = rsa_sign(&sk, b"message").expect("sign");
        assert!(rsa_verify(&pk, b"message", &sig));
        assert!(!rsa_verify(&pk, b"tampered", &sig));
        assert!(!rsa_verify(&pk, b"message", &"00".repeat(256)));
        assert!(rsa_sign("junk", b"message").is_err());
    }

    #[test]
    fn jwt_hs256_roundtrip_and_attacks() {
        let secret = "topsecret";
        let payload = r#"{"sub":"123","name":"Ada"}"#;
        let tok = jwt_encode(payload, secret);
        assert_eq!(tok.split('.').count(), 3);
        assert_eq!(jwt_decode(&tok, secret).expect("decode"), payload);
        // Wrong secret.
        assert!(jwt_decode(&tok, "wrong").is_err());
        // Tampered payload (re-encode "Eve" under same shape).
        let parts: Vec<&str> = tok.split('.').collect();
        let evil = b64url_encode(br#"{"sub":"123","name":"Eve"}"#);
        let tampered = format!("{}.{}.{}", parts[0], evil, parts[2]);
        assert!(jwt_decode(&tampered, secret).is_err());
        // Malformed tokens.
        assert!(jwt_decode("a.b", secret).is_err());
        assert!(jwt_decode("a.b.c.d", secret).is_err());
        assert!(jwt_decode("", secret).is_err());
        // Expired vs future exp.
        let past = r#"{"sub":"1","exp":1}"#;
        assert!(jwt_decode(&jwt_encode(past, secret), secret).is_err());
        let future = format!(r#"{{"sub":"1","exp":{}}}"#, unix_now() + 3600);
        assert_eq!(
            jwt_decode(&jwt_encode(&future, secret), secret).expect("decode"),
            future
        );
    }

    #[test]
    fn jwt_eddsa_roundtrip() {
        let (sk, pk) = ed25519_keypair();
        let payload = r#"{"sub":"7"}"#;
        let tok = jwt_encode_ed(payload, &sk).expect("encode");
        assert_eq!(jwt_decode_ed(&tok, &pk).expect("decode"), payload);
        let (_, other_pk) = ed25519_keypair();
        assert!(jwt_decode_ed(&tok, &other_pk).is_err());
        assert!(jwt_encode_ed(payload, "zz").is_err());
    }
}
