//! `std.crypto` password hashing for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! ([`argon2_hash`], [`argon2_verify`], [`bcrypt_hash`], [`bcrypt_verify`])
//! through thin `zz_stdlib` adapters, while AOT binaries call the
//! `extern "C"` `zz_crypto_*` functions (same `zz_value` convention).
//!
//! Security contract:
//! - Hashing uses OWASP-recommended defaults: Argon2id (m=19 MiB, t=2,
//!   p=1) and bcrypt cost 12. Slow by design — fixtures hash once.
//! - Salts are fresh CSPRNG per hash (two hashes of one password differ).
//! - Verification returns `bool` and never leaks *why* it failed: wrong
//!   password, malformed PHC string, and unsupported params all yield
//!   `false`. No panics on hostile input.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

use crate::cabi::{cvalue_str, cvalue_to_string, CValue};

/// Argon2id hash of `password` as a PHC string (`$argon2id$v=19$…`).
/// Random salt per call. Errors only on OS RNG failure (effectively
/// unreachable); invalid input cannot occur (`&[u8]` always hashes).
pub fn argon2_hash(password: &[u8]) -> Result<String, String> {
    let salt = SaltString::generate(&mut rand_core::OsRng);
    Argon2::default()
        .hash_password(password, &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("crypto.argon2_hash failed: {e}"))
}

/// True when `password` matches the PHC `hash`. `false` for wrong
/// passwords AND for malformed/unsupported hash strings.
pub fn argon2_verify(hash: &str, password: &[u8]) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default().verify_password(password, &parsed).is_ok()
}

/// Bcrypt hash of `password` (`$2b$12$…`). Random salt per call.
pub fn bcrypt_hash(password: &[u8]) -> Result<String, String> {
    // bcrypt speaks UTF-8 passwords; reject non-UTF-8 rather than
    // lossy-converting a secret.
    let Ok(pw) = std::str::from_utf8(password) else {
        return Err("crypto.bcrypt_hash: password must be valid UTF-8".to_string());
    };
    bcrypt::hash(pw, bcrypt::DEFAULT_COST).map_err(|e| format!("crypto.bcrypt_hash failed: {e}"))
}

/// True when `password` matches bcrypt `hash`; `false` otherwise
/// (including malformed hashes).
pub fn bcrypt_verify(hash: &str, password: &[u8]) -> bool {
    let Ok(pw) = std::str::from_utf8(password) else {
        return false;
    };
    bcrypt::verify(pw, hash).unwrap_or(false)
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

/// `crypto.argon2_hash(pw: str) -> str` (empty + err on RNG failure).
#[no_mangle]
pub extern "C" fn zz_crypto_argon2_hash(pw: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(pw) {
        // SAFETY: constructors are provided by the linked AOT program.
        Some(p) => match argon2_hash(p.as_bytes()) {
            Ok(h) => cvalue_str(h.as_bytes()),
            Err(_) => {
                set_err(err);
                cvalue_str(b"")
            }
        },
        None => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `crypto.argon2_verify(hash: str, pw: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_crypto_argon2_verify(
    hash: CValue,
    pw: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (cvalue_to_string(hash), cvalue_to_string(pw)) {
        (Some(h), Some(p)) => CValue::boolean(argon2_verify(&h, p.as_bytes())),
        _ => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `crypto.bcrypt_hash(pw: str) -> str` (empty + err on failure).
#[no_mangle]
pub extern "C" fn zz_crypto_bcrypt_hash(pw: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(pw) {
        // SAFETY: constructors are provided by the linked AOT program.
        Some(p) => match bcrypt_hash(p.as_bytes()) {
            Ok(h) => cvalue_str(h.as_bytes()),
            Err(_) => {
                set_err(err);
                cvalue_str(b"")
            }
        },
        None => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `crypto.bcrypt_verify(hash: str, pw: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_crypto_bcrypt_verify(
    hash: CValue,
    pw: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (cvalue_to_string(hash), cvalue_to_string(pw)) {
        (Some(h), Some(p)) => CValue::boolean(bcrypt_verify(&h, p.as_bytes())),
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
    fn argon2_roundtrip_and_salt_uniqueness() {
        let h1 = argon2_hash(b"correct horse").expect("hash");
        let h2 = argon2_hash(b"correct horse").expect("hash");
        assert!(h1.starts_with("$argon2id$"), "PHC format: {h1}");
        assert_ne!(h1, h2, "fresh salt per hash");
        assert!(argon2_verify(&h1, b"correct horse"));
        assert!(!argon2_verify(&h1, b"wrong horse"));
        assert!(!argon2_verify("not-a-hash", b"correct horse"));
        assert!(!argon2_verify("", b"correct horse"));
        // Cross-algorithm string must not verify.
        assert!(!argon2_verify(
            "$2b$12$Lr5cNl2hdKOiHZx9QyE5eMiXJv5m6y7u8i9o0p1q2r3s4t5u6",
            b"correct horse"
        ));
    }

    #[test]
    fn bcrypt_roundtrip_and_salt_uniqueness() {
        let h1 = bcrypt_hash(b"s3cret!").expect("hash");
        let h2 = bcrypt_hash(b"s3cret!").expect("hash");
        assert!(h1.starts_with("$2b$12$"), "PHC format: {h1}");
        assert_ne!(h1, h2, "fresh salt per hash");
        assert!(bcrypt_verify(&h1, b"s3cret!"));
        assert!(!bcrypt_verify(&h1, b"n0pe!"));
        assert!(!bcrypt_verify("not-a-hash", b"s3cret!"));
    }
}
