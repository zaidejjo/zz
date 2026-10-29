//! Windows-only link fallbacks for the AOT C runtime symbols.
//!
//! See `cabi_fallback.c`: the Rust FFI entry points in this crate call the
//! *AOT program's* C runtime (`zz_str_new`, `zz_array_new`, …). Those are
//! resolved by the C runtime that `zz build` links next to this archive, but
//! a plain `cargo test` binary links the rlib without it. ELF/macOS linkers
//! drop the unreferenced entry points; MSVC keeps them and fails with
//! `LNK2019` for every constructor.
//!
//! The shim is a Windows-only, whole-archive native library: whole-archive
//! matters because `/alternatename` directives only take effect when the
//! object carrying them is linked, and nothing would otherwise reference it.
//! It defines no runtime symbol, so a real AOT definition always wins.
//!
//! Compilation failures are reported as a warning instead of failing the
//! build: a missing C toolchain must not block a plain Rust build, and the
//! original link error then stays visible where it matters (test links).

fn main() {
    println!("cargo:rerun-if-changed=cabi_fallback.c");
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let mut build = cc::Build::new();
    build
        .file("cabi_fallback.c")
        .warnings(false)
        // We emit the link directives ourselves so the archive can be
        // whole-archived (cc's default `-l static=` would let the linker
        // drop the object, and the `/alternatename` fallbacks with it).
        .cargo_metadata(false);
    // `try_compile` writes to OUT_DIR and reports an error when no usable C
    // compiler is present (e.g. a bare Rust-only Windows install).
    if let Err(_e) = build.try_compile("zz_cabi_fallback") {
        println!(
            "cargo:warning=zz_native_rt: no C compiler for the Windows C-ABI \
             fallback shim; test binaries that link the AOT entry points may \
             fail with LNK2019"
        );
        return;
    }
    let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
    println!("cargo:rustc-link-search=native={out_dir}");
    // Whole-archive: the object must be in the link for the fallbacks to be
    // registered, and nothing references it otherwise.
    println!("cargo:rustc-link-lib=static:+whole-archive=zz_cabi_fallback");
}
