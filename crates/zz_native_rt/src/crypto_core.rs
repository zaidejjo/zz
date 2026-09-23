//! `std.crypto` core primitives for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! ([`sha256_hex`], [`sha512_hex`], [`hmac_sha256_hex`], [`random_hex`],
//! [`ct_eq`]) through thin `zz_stdlib` adapters, while AOT binaries call
//! the `extern "C"` `zz_crypto_*` functions (same `zz_value` convention as
//! embedded-C natives).
//!
//! Design notes:
//! - Digests cross as lowercase hex strings (deterministic, printable).
//! - `random_bytes` returns **hex of `n` CSPRNG bytes** (`getrandom`, the
//!   same source `OsRng` draws from). Raw bytes cannot cross as `str`
//!   values because `str` must be valid UTF-8; hex keeps VM and AOT shapes
//!   identical. Requested lengths are capped (`MAX_RANDOM_BYTES`) so a
//!   hostile `n` cannot OOM the process.
//! - `ct_eq` compares with `subtle` (no early exit on content). Lengths
//!   are public by design (standard practice: length is rarely secret).
//! - Explicit `zeroize` surface is deferred to the password phase, where
//!   secret-key handles exist; pool payloads are dropped (not zeroed) here.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha512};
use subtle::ConstantTimeEq;

use crate::cabi::{cvalue_str, cvalue_to_string, CValue};

/// Upper bound for a single `random_bytes` request (1 MiB of entropy).
pub const MAX_RANDOM_BYTES: usize = 1024 * 1024;

fn hex(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(ALPHABET[(b >> 4) as usize] as char);
        out.push(ALPHABET[(b & 0x0f) as usize] as char);
    }
    out
}

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// SHA-512 of `data`, lowercase hex.
pub fn sha512_hex(data: &[u8]) -> String {
    hex(&Sha512::digest(data))
}

/// HMAC-SHA-256 of `msg` under `key`, lowercase hex.
pub fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    hex(&mac.finalize().into_bytes())
}

/// Hex of `n` fresh CSPRNG bytes. Errors on `n > MAX_RANDOM_BYTES` (and on
/// OS RNG failure, which is effectively unreachable on hosted platforms).
pub fn random_hex(n: usize) -> Result<String, String> {
    if n > MAX_RANDOM_BYTES {
        return Err(format!(
            "crypto.random_bytes: requested {n} bytes, max is {MAX_RANDOM_BYTES}"
        ));
    }
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| format!("crypto.random_bytes: OS RNG failed: {e}"))?;
    Ok(hex(&buf))
}

/// Constant-time equality. Never panics; false on any mismatch (including
/// length mismatch).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

/// `crypto.sha256(s: str) -> str`.
#[no_mangle]
pub extern "C" fn zz_crypto_sha256(s: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(s) {
        Some(text) => cvalue_str(sha256_hex(text.as_bytes()).as_bytes()),
        None => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `crypto.sha512(s: str) -> str`.
#[no_mangle]
pub extern "C" fn zz_crypto_sha512(s: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(s) {
        Some(text) => cvalue_str(sha512_hex(text.as_bytes()).as_bytes()),
        None => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `crypto.hmac_sha256(key: str, msg: str) -> str`.
#[no_mangle]
pub extern "C" fn zz_crypto_hmac_sha256(
    key: CValue,
    msg: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (cvalue_to_string(key), cvalue_to_string(msg)) {
        // SAFETY: constructors are provided by the linked AOT program.
        (Some(k), Some(m)) => cvalue_str(hmac_sha256_hex(k.as_bytes(), m.as_bytes()).as_bytes()),
        _ => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `crypto.sha256_bytes(b: bytes) -> str` (hex over raw bytes).
#[no_mangle]
pub extern "C" fn zz_crypto_sha256_bytes(b: CValue, err: *mut std::ffi::c_int) -> CValue {
    use crate::cabi::cvalue_to_bytes;
    match cvalue_to_bytes(b) {
        Some(data) => cvalue_str(sha256_hex(&data).as_bytes()),
        None => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `crypto.random_bytes(n: int) -> str` (hex of `n` CSPRNG bytes; `""` and
/// `*err = 1` when out of range).
#[no_mangle]
pub extern "C" fn zz_crypto_random_bytes(n: CValue, err: *mut std::ffi::c_int) -> CValue {
    match n.as_i64() {
        Some(count) if count >= 0 => match random_hex(count as usize) {
            Ok(h) => cvalue_str(h.as_bytes()),
            Err(_) => {
                set_err(err);
                cvalue_str(b"")
            }
        },
        _ => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `crypto.ct_eq(a: str, b: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_crypto_ct_eq(a: CValue, b: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (cvalue_to_string(a), cvalue_to_string(b)) {
        (Some(x), Some(y)) => CValue::boolean(ct_eq(x.as_bytes(), y.as_bytes())),
        _ => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_answer() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha512_known_answer() {
        assert_eq!(
            sha512_hex(b"abc"),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn hmac_rfc4231_case2() {
        // RFC 4231 §4.2, HMAC-SHA-256.
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn random_hex_shape_and_uniqueness() {
        let a = random_hex(16).expect("rng");
        let b = random_hex(16).expect("rng");
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "CSPRNG output must not repeat");
        assert_eq!(random_hex(0).expect("rng"), "");
        assert!(random_hex(MAX_RANDOM_BYTES + 1).is_err());
    }

    #[test]
    fn ct_eq_semantics() {
        assert!(ct_eq(b"same", b"same"));
        assert!(!ct_eq(b"same", b"saMe"));
        assert!(!ct_eq(b"short", b"longer"));
        assert!(ct_eq(b"", b""));
    }
}
