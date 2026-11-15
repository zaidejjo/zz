//! GENERATED FILE - do not edit by hand.
//!
//! Thin shims over the single declarative source in `zz_stdlib_meta`.
//! Regenerate with `cargo run -p zz_stdlib_meta --bin regen`.

/// Map a zz native qualified name to its C runtime implementation name.
pub fn native_impl(name: &str) -> Option<&'static str> {
    zz_stdlib_meta::native_impl(name)
}

/// Map a zz native qualified name to its Rust-staticlib C symbol.
pub fn ffi_impl(name: &str) -> Option<&'static str> {
    zz_stdlib_meta::ffi_impl(name)
}

/// C declaration for a staticlib symbol.
pub fn ffi_decl(symbol: &str) -> Option<&'static str> {
    zz_stdlib_meta::ffi_decl(symbol)
}
