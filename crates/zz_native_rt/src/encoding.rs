//! `std.encoding` FFI natives (AOT side).
//!
//! Mirrors the VM natives in `zz_stdlib::natives::encoding` through the
//! same `zz_value fn(args…, int *err)` convention.

use crate::cabi::{cvalue_bytes, cvalue_str, cvalue_to_string, CValue};

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

/// `encoding.base64_decode_bytes(s: str) -> Result<bytes, str>`.
///
/// Unlike `base64_decode` (lossy UTF-8), raw bytes cross intact for
/// binary payloads (tarballs). Errors surface as `Err(str)`.
#[no_mangle]
pub extern "C" fn zz_encoding_base64_decode_bytes(s: CValue, err: *mut std::ffi::c_int) -> CValue {
    use crate::cabi::{zz_variant_err, zz_variant_ok};
    use base64::Engine;
    match cvalue_to_string(s) {
        Some(text) => {
            match base64::engine::general_purpose::STANDARD.decode(text.as_bytes()) {
                // SAFETY: constructors are provided by the linked AOT program.
                Ok(raw) => unsafe { zz_variant_ok(cvalue_bytes(&raw)) },
                Err(e) => unsafe {
                    zz_variant_err(cvalue_str(format!("base64 decode error: {e}").as_bytes()))
                },
            }
        }
        None => {
            set_err(err);
            // SAFETY: constructors are provided by the linked AOT program.
            unsafe { zz_variant_err(cvalue_str(b"encoding.base64_decode_bytes: expected a str")) }
        }
    }
}
