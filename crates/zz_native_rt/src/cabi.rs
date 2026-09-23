//! C ABI mirror of the AOT `zz_value` (see `zz_codegen/src/runtime/core.h`).
//!
//! FFI natives speak the same calling convention as embedded-C natives:
//! `zz_value fn(zz_value, …, int *err)`, invoked through the existing
//! `zz_call_nativeN` shims with zero emission changes. Handles cross as
//! `ZZ_INT` ids into the process pool; strings are read via the linkable
//! `zz_str_view` helper and built via `zz_str_new`; arrays via
//! `zz_array_new`/`zz_array_push`; variants via `zz_variant_some/ok/err`.
//!
//! Only the reprs actually used by FFI natives are mirrored here. Reading
//! is layout-safe by construction: tag constants are asserted against the
//! C enum order in the link test, and string bytes always go through
//! `zz_str_view` (never a Rust-side struct mirror).

use std::ffi::c_void;

/// `zz_value` by value: 4-byte tag + 4-byte pad + 8-byte payload (16 total).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CValue {
    /// `zz_tag` discriminant (`TAG_*` below).
    pub tag: u32,
    /// Explicit pad (C inserts 4 bytes before the 8-byte union).
    pub _pad: u32,
    /// Payload: int bits, float bits, bool byte, or a pointer address.
    pub payload: u64,
}

const _: () = assert!(std::mem::size_of::<CValue>() == 16);

/// Tag constants mirroring the C `zz_tag` enum order.
pub const TAG_UNIT: u32 = 0;
pub const TAG_INT: u32 = 1;
pub const TAG_FLOAT: u32 = 2;
pub const TAG_BOOL: u32 = 3;
pub const TAG_STR: u32 = 4;
pub const TAG_ARRAY: u32 = 5;
/// `ZZ_OPTION_NONE` (10): constructed directly — no linkable `none`
/// constructor exists, and the payload is always zero.
pub const TAG_OPTION_NONE: u32 = 10;
/// `ZZ_BYTES` (24): byte buffer. Reads go through [`zz_bytes_view`];
/// construction through [`zz_bytes_new`].
pub const TAG_BYTES: u32 = 24;

impl CValue {
    /// Build a `ZZ_UNIT` value.
    pub fn unit() -> Self {
        CValue {
            tag: TAG_UNIT,
            _pad: 0,
            payload: 0,
        }
    }

    /// Build a `ZZ_INT` value.
    pub fn int(i: i64) -> Self {
        CValue {
            tag: TAG_INT,
            _pad: 0,
            payload: i as u64,
        }
    }

    /// Build a `ZZ_BOOL` value.
    pub fn boolean(b: bool) -> Self {
        CValue {
            tag: TAG_BOOL,
            _pad: 0,
            payload: u64::from(b),
        }
    }

    /// The `ZZ_INT` payload as `i64` (`None` for other tags).
    pub fn as_i64(self) -> Option<i64> {
        (self.tag == TAG_INT).then_some(self.payload as i64)
    }

    /// A `ZZ_OPTION_NONE` value (nullary variant, zero payload).
    pub fn none() -> Self {
        CValue {
            tag: TAG_OPTION_NONE,
            _pad: 0,
            payload: 0,
        }
    }

    /// The `ZZ_ARRAY` payload as a raw pointer (`None` for other tags).
    pub fn as_array_ptr(self) -> Option<*mut c_void> {
        (self.tag == TAG_ARRAY).then_some(self.payload as *mut c_void)
    }
}

extern "C" {
    /// Copy-on-write string constructor (linkable from the AOT program).
    pub fn zz_str_new(s: *const u8, len: usize) -> CValue;
    /// Empty array constructor.
    pub fn zz_array_new() -> CValue;
    /// Push (clones ARC payloads as needed).
    pub fn zz_array_push(a: *mut c_void, v: CValue);
    /// Array header length (null-safe on the C side).
    pub fn zz_array_len(a: *const c_void) -> usize;
    /// Option/Result variant constructors.
    pub fn zz_variant_some(inner: CValue) -> CValue;
    pub fn zz_variant_ok(inner: CValue) -> CValue;
    pub fn zz_variant_err(inner: CValue) -> CValue;
    /// Read string bytes: sets `(*out_ptr, *out_len)`; null/0 for non-strings.
    pub fn zz_str_view(v: CValue, out_ptr: *mut *const u8, out_len: *mut usize);
    /// Borrow a `ZZ_BYTES` window: sets `(*out_ptr, *out_len)`; null/0 for
    /// non-bytes. The view borrows the shared store — copy before use.
    pub fn zz_bytes_view(v: CValue, out_ptr: *mut *const u8, out_len: *mut usize);
    /// Build a `ZZ_BYTES` by copying `len` bytes.
    pub fn zz_bytes_new(data: *const u8, len: usize) -> CValue;
    /// Positional array read (cloned item, or unit when out of range).
    /// The caller owns the clone and must release it with [`zz_release`].
    pub fn zz_array_at(arr: CValue, i: usize) -> CValue;
    /// Release one owned `zz_value` (balances a clone from `zz_array_at`).
    pub fn zz_value_release(v: CValue);
}

/// Build a `ZZ_STR` from Rust bytes via the runtime allocator.
pub fn cvalue_str(s: &[u8]) -> CValue {
    // SAFETY: `zz_str_new` copies `len` bytes synchronously.
    unsafe { zz_str_new(s.as_ptr(), s.len()) }
}

/// Read a `ZZ_STR` argument into an owned Rust `String`.
///
/// Returns `None` for non-string values and for invalid UTF-8 (zz strings
/// are byte buffers in AOT; only valid UTF-8 crosses into regex/crypto).
pub fn cvalue_to_string(v: CValue) -> Option<String> {
    if v.tag != TAG_STR {
        return None;
    }
    let mut ptr: *const u8 = std::ptr::null();
    let mut len: usize = 0;
    // SAFETY: out-params are valid locals; the call only reads `v`.
    unsafe { zz_str_view(v, &mut ptr, &mut len) };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: `zz_str_view` guarantees `len` readable bytes for this call;
    // we copy to an owned `String` before returning.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

/// Read a `ZZ_BYTES` argument into owned Rust bytes (`None` for non-bytes).
pub fn cvalue_to_bytes(v: CValue) -> Option<Vec<u8>> {
    if v.tag != TAG_BYTES {
        return None;
    }
    let mut ptr: *const u8 = std::ptr::null();
    let mut len: usize = 0;
    // SAFETY: out-params are valid locals; the call only reads `v`.
    unsafe { zz_bytes_view(v, &mut ptr, &mut len) };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: `zz_bytes_view` guarantees `len` readable bytes for this call;
    // we copy before returning.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

/// Build a `ZZ_BYTES` from Rust bytes via the runtime allocator.
pub fn cvalue_bytes(data: &[u8]) -> CValue {
    // SAFETY: `zz_bytes_new` copies `len` bytes synchronously.
    unsafe { zz_bytes_new(data.as_ptr(), data.len()) }
}

/// Push a Rust string into a `ZZ_ARRAY` built by [`zz_array_new`].
pub fn array_push_str(arr: CValue, s: &str) {
    if let Some(ptr) = arr.as_array_ptr() {
        // SAFETY: `ptr` came from a `zz_array_new` value in this call frame.
        unsafe { zz_array_push(ptr, cvalue_str(s.as_bytes())) };
    }
}

/// Read a `ZZ_ARRAY` of strings into owned Rust strings.
///
/// Non-string items become `""` (never fail the whole read on one bad
/// item); non-arrays yield `None`. Every clone taken through
/// `zz_array_at` is released after conversion, so no refs leak.
pub fn cvalue_to_str_vec(v: CValue) -> Option<Vec<String>> {
    if v.tag != TAG_ARRAY {
        return None;
    }
    let ptr = v.as_array_ptr()?;
    // SAFETY: `zz_array_len` only reads the header through a valid pointer.
    let n = unsafe { zz_array_len(ptr) };
    let mut out = Vec::with_capacity(n.min(1_000_000));
    for i in 0..n.min(1_000_000) {
        // SAFETY: `i` is in range; the clone is ours to release.
        let item = unsafe { zz_array_at(v, i) };
        let s = cvalue_to_string(item).unwrap_or_default();
        // SAFETY: balances the clone above.
        unsafe { zz_value_release(item) };
        out.push(s);
    }
    Some(out)
}
