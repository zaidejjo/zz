//! FNV-1a 64-bit hash as a zz native plugin.
//!
//! No `build.rs`, no scripts, no path dependencies: `zz` vendors the
//! committed lockfile closure (empty here) and drives
//! `cargo build --offline --locked` itself.
//!
//! The manifest is `// C-ABI: 1`, so the symbols below are looked up
//! directly through the C plugin path on both engines. The ABI stamp is
//! therefore the C one (`ZZ_C_PLUGIN_ABI_VERSION`), exported exactly once.

use std::os::raw::c_char;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// ABI version stamp consumed by the VM loader for `// C-ABI: 1`.
#[no_mangle]
pub static ZZ_C_PLUGIN_ABI_VERSION: u32 = 1;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Hash a borrowed, NUL-terminated string.
///
/// # Safety
/// `s` must be a valid NUL-terminated C string for the duration of the
/// call (the runtime guarantees this for `str` params).
#[no_mangle]
pub unsafe extern "C" fn fnv_hash(s: *const c_char) -> i64 {
    if s.is_null() {
        return FNV_OFFSET_BASIS as i64;
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes();
    fnv1a(bytes) as i64
}

/// FNV-1a offset basis.
#[no_mangle]
pub extern "C" fn fnv_seed() -> i64 {
    FNV_OFFSET_BASIS as i64
}
