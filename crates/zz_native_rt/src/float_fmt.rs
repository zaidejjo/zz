//! Canonical float formatting (IR spec §4).
//!
//! The single source of truth for how `f64` values render in Display
//! positions (`println`, interpolation, `str()`). The VM computes this
//! inline (`zz_runtime::value::Value` Display); the AOT backend MUST NOT
//! reimplement it in C — it calls [`zz_float_format_raw`] through the C
//! ABI instead, so both engines share one implementation and one
//! observable behavior.
//!
//! The rule (mirrors the VM exactly — see the comment on the branch
//! below; if the VM ever changes, this function changes with it and the
//! `edge_float_format` conformance fixture pins both):
//!
//! - finite + integral (`fract() == 0`, includes `±0`): `{x:.1}`
//!   (`1.0`, `-0.0`, `1000000000000000000000.0` — never exponent).
//! - anything else: `{x}` (shortest round-trip, never exponent).
//! - special values fall out of Rust `Display`: `NaN`, `inf`, `-inf`.

use std::ffi::{c_char, c_double};

/// Format `x` per the canonical rule into the caller's buffer.
///
/// C ABI: writes the UTF-8 bytes plus a NUL terminator when `cap` fits;
/// returns the byte length **excluding** the NUL. When `buf` is null,
/// `cap` is 0, or the buffer is too small (`cap <= len`), nothing is
/// written and the needed length is returned, so C can retry with a heap
/// buffer (`char stack[1024]` covers every `f64`; the retry is paranoia
/// for future widths, never a hot path).
///
/// Never fails: every `f64` has a `Display` rendering.
///
/// # Safety
///
/// When `buf` is non-null, it must point to `cap` writable bytes. The
/// C backend upholds this (stack `[u8; 1024]` or a heap buffer sized
/// from a prior query call).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn zz_float_format_raw(x: c_double, buf: *mut c_char, cap: usize) -> usize {
    // MUST match `zz_runtime::value` float Display arm-for-arm:
    // `fract() == 0.0` (not `== x.trunc()`: identical, but keep the
    // spelling so the two sites stay visually diffable).
    let s = if x.is_finite() && x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    };
    let b = s.as_bytes();
    if buf.is_null() || cap == 0 || cap <= b.len() {
        return b.len();
    }
    // SAFETY: caller guarantees `buf` points to `cap` writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(b.as_ptr() as *const c_char, buf, b.len());
        *buf.add(b.len()) = 0;
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(x: f64) -> String {
        let mut buf = vec![0 as c_char; 1024];
        let n = zz_float_format_raw(x, buf.as_mut_ptr(), buf.len());
        assert!(n < buf.len());
        String::from_utf8(buf[..n].iter().map(|&c| c as u8).collect()).unwrap()
    }

    #[test]
    fn conformance_vectors() {
        // Recorded VM ground truth (2026-10-05); `edge_float_format`
        // pins the same vectors end to end on both engines.
        assert_eq!(fmt(0.1), "0.1");
        assert_eq!(fmt(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(fmt(1.0), "1.0");
        assert_eq!(fmt(-0.0), "-0.0");
        assert_eq!(fmt(0.0), "0.0");
        assert_eq!(fmt(1e21), "1000000000000000000000.0");
        assert_eq!(fmt(1e-7), "0.0000001");
        assert_eq!(fmt(2.5), "2.5");
        assert_eq!(fmt(100.0), "100.0");
        assert_eq!(fmt(f64::NAN), "NaN");
        assert_eq!(fmt(f64::INFINITY), "inf");
        assert_eq!(fmt(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn extremes_have_no_exponent() {
        let max = fmt(f64::MAX);
        assert!(max.ends_with(".0"), "{max}");
        assert!(!max.contains(['e', 'E']), "{max}");
        assert_eq!(max.len(), 311, "MAX renders as 309 digits + .0");
        let min = fmt(f64::from_bits(1));
        assert!(!min.contains(['e', 'E']), "{min}");
        assert!(min.starts_with("0.000"), "{min}");
        assert!(min.ends_with('5'), "{min}");
        assert_eq!(min.len(), 326, "5e-324 renders as 0. + 323 chars");
    }

    #[test]
    fn query_mode_reports_length_without_writing() {
        assert_eq!(zz_float_format_raw(1.0, std::ptr::null_mut(), 0), 3);
        let mut sentinel = [1 as c_char; 4];
        // Too small (cap <= len): no write, length reported.
        assert_eq!(zz_float_format_raw(1.0, sentinel.as_mut_ptr(), 3), 3);
        assert!(sentinel.iter().all(|&c| c == 1));
    }
}
