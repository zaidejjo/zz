//! `std.uuid` identifier generation for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! ([`v4`], [`v7`], [`parse`], [`is_valid`]) through thin `zz_stdlib`
//! adapters, while AOT binaries call the `extern "C"` `zz_uuid_*`
//! functions (same `zz_value` convention).
//!
//! UUIDs cross as canonical hyphenated lowercase strings
//! (`8-4-4-4-12`); no handles are involved. `v7` is time-ordered
//! (lexicographically sortable within one process clock tick).

use crate::cabi::{cvalue_str, cvalue_to_string, CValue};

/// Random (v4) UUID string.
pub fn v4() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

/// Time-ordered (v7) UUID string.
pub fn v7() -> String {
    uuid::Uuid::now_v7().hyphenated().to_string()
}

/// Normalize `s` to canonical form. Accepts hyphenated, simple (32 hex),
/// urn, and braced spellings, any case; rejects everything else.
pub fn parse(s: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(s)
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| format!("uuid.parse: invalid UUID `{s}`"))
}

/// True when `s` parses as any UUID version.
pub fn is_valid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok()
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

/// `uuid.v4() -> str`.
#[no_mangle]
pub extern "C" fn zz_uuid_v4(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    cvalue_str(v4().as_bytes())
}

/// `uuid.v7() -> str`.
#[no_mangle]
pub extern "C" fn zz_uuid_v7(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    cvalue_str(v7().as_bytes())
}

/// `uuid.parse(s: str) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_uuid_parse(s: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(s) {
        // SAFETY: constructors are provided by the linked AOT program.
        Some(text) => match parse(&text) {
            Ok(u) => unsafe { crate::cabi::zz_variant_ok(cvalue_str(u.as_bytes())) },
            Err(e) => unsafe { crate::cabi::zz_variant_err(cvalue_str(e.as_bytes())) },
        },
        None => {
            set_err(err);
            // SAFETY: constructors are provided by the linked AOT program.
            unsafe { crate::cabi::zz_variant_err(cvalue_str(b"uuid.parse: want str")) }
        }
    }
}

/// `uuid.is_valid(s: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_uuid_is_valid(s: CValue, err: *mut std::ffi::c_int) -> CValue {
    match cvalue_to_string(s) {
        Some(text) => CValue::boolean(is_valid(&text)),
        None => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_shape_and_uniqueness() {
        let a = v4();
        let b = v4();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4", "version nibble: {a}");
        assert!(is_valid(&a));
        assert_ne!(a, b);
    }

    #[test]
    fn v7_shape_and_ordering() {
        let a = v7();
        let b = v7();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "7", "version nibble: {a}");
        assert!(is_valid(&a));
        // Time-ordered: later stamp sorts no earlier (same-ms ties equal).
        assert!(b >= a, "{a} then {b}");
    }

    #[test]
    fn parse_normalizes_and_rejects() {
        // The well-known example v4 UUID.
        let canon = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(parse(canon).expect("parse"), canon);
        assert_eq!(
            parse("550E8400-E29B-41D4-A716-446655440000").expect("upper"),
            canon
        );
        assert_eq!(
            parse("550e8400e29b41d4a716446655440000").expect("simple"),
            canon
        );
        assert!(parse("junk").is_err());
        assert!(parse("").is_err());
        assert!(parse("550e8400-e29b-41d4-a716").is_err());
        assert!(!is_valid("junk"));
        assert!(!is_valid(""));
    }
}
