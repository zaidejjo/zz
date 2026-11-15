//! Embedded-C native resolution (delegates to `zz_stdlib_meta`).
//!
//! The table itself lives in `zz_stdlib_meta::c_impl`, next to the signature
//! and FFI tables, so the AOT glue is maintained in one crate. This module
//! keeps the historical `zz_codegen_ffi::natives` path and the combined
//! "has a callable implementation" predicate.

pub use crate::generated::native_impl;

/// Whether a reachable native has a callable implementation.
///
/// Embedded-C natives resolve through [`native_impl`]; Rust-staticlib
/// natives (regex, crypto, ...) through [`crate::ffi_impl`]. Anything else
/// lowers to unit (documented MVP limitation).
pub fn native_supported(name: &str) -> bool {
    native_impl(name).is_some() || crate::ffi_impl(name).is_some()
}
